//! `ti serve` - a small web view of the repo's tickets.
//!
//! Shows the same thing the TUI's issue list does (id, age, priority,
//! title, tags) plus a per-ticket detail page, served over plain HTTP
//! from a hand-rolled `std::net` listener so we pull in no web stack.
//!
//! By default the view is read-only. Pass `--edit` to enable inline
//! mutation forms on the detail page (comment, state, assign, priority,
//! tags). Every mutation is a POST that runs the same `TicketStore`
//! calls the CLI does, then redirects back to the ticket (PRG).
//!
//! The ticket pages live in [`tickets`]; the POST handlers live in
//! [`edit`]; this module owns the listener, the request/response
//! plumbing, and the shared page chrome.

mod edit;
mod kanban;
mod flow;
mod tickets;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ticgit_lib::TicketStore;
use time::OffsetDateTime;

use crate::commands::open_store;
use crate::render::{self, NickMap};

/// How long a client gets to send its request line and headers.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on the request line + headers we're willing to read.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Cap on a POST body (comments/specs are small).
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Parser)]
pub struct Args {
    /// Port to listen on. Use 0 to pick a free port.
    #[arg(short = 'p', long = "port", default_value_t = 8177)]
    pub port: u16,

    /// Address to bind. Defaults to localhost only.
    #[arg(long = "bind", default_value = "127.0.0.1")]
    pub bind: String,

    /// Open the served page in your browser.
    #[arg(long = "open")]
    pub open: bool,

    /// Enable inline edit forms on the detail page. Off by default;
    /// the server is read-only without this.
    #[arg(long = "edit")]
    pub edit: bool,
}

/// Per-server state shared by every request: whether edits are allowed
/// and the CSRF token that every edit form must echo back.
struct Server {
    edit: bool,
    csrf: String,
}

pub fn run(args: Args) -> Result<()> {
    // Fail early (and with the usual error) if we're not in a ticgit repo.
    let store = open_store()?;
    drop(store);

    let server = Server {
        edit: args.edit,
        csrf: generate_csrf_token(),
    };
    if args.edit {
        println!("ti serve: edit mode is ON — mutations will write to the ticgit store");
    }

    let listener = TcpListener::bind((args.bind.as_str(), args.port))
        .with_context(|| format!("binding {}:{}", args.bind, args.port))?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/");
    println!("ti serve: listening on {url} (ctrl-c to stop)");
    if args.open {
        open_browser(&url);
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(stream, &server) {
                    eprintln!("ti serve: {err:#}");
                }
            }
            Err(err) => eprintln!("ti serve: accept failed: {err}"),
        }
    }
    Ok(())
}

/// A random-ish token good enough for localhost CSRF. We don't need
/// cryptographic strength; we need something a cross-site request
/// can't guess, generated once per server start.
fn generate_csrf_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ (std::process::id() as u128);
    format!("{seed:032x}")
}

fn handle_connection(mut stream: TcpStream, server: &Server) -> Result<()> {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));

    let request = match read_request(&stream)? {
        Some(request) => request,
        None => return Ok(()),
    };

    let response = match route(&request, server) {
        Ok(response) => response,
        Err(err) => Response::html(500, error_page("500 - server error", &format!("{err:#}"))),
    };
    response.write_to(&mut stream)
}

/// A parsed request: everything we care about from the client. The
/// `body` is the raw bytes of a POST (form-urlencoded); `form` parses
/// it on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    method: String,
    path: String,
    params: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn param_values(&self, key: &str) -> Vec<String> {
        self.params
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn flag(&self, key: &str) -> bool {
        matches!(self.param(key), Some("1" | "true" | "yes" | ""))
    }

    /// Parsed `application/x-www-form-urlencoded` body. Empty for GET.
    fn form(&self) -> Vec<(String, String)> {
        let body = std::str::from_utf8(&self.body).unwrap_or("");
        parse_query(body)
    }

    /// First value for a form field, trimmed.
    fn form_field(&self, key: &str) -> Option<String> {
        self.form()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

fn read_request(stream: &TcpStream) -> Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    // Collect headers so the client doesn't see a reset before our
    // response, and so we can read Content-Length for POST bodies.
    let mut read = line.len();
    let mut content_length: Option<usize> = None;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        read += n;
        if n == 0 || header == "\r\n" || header == "\n" || read > MAX_HEADER_BYTES {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(len) = value.trim().parse::<usize>() {
                    content_length = Some(len.min(MAX_BODY_BYTES));
                }
            }
        }
    }

    let mut body = Vec::new();
    if let Some(len) = content_length {
        body.resize(len, 0);
        reader.read_exact(&mut body)?;
    }

    Ok(parse_request_line(&line).map(|mut req| {
        req.body = body;
        req
    }))
}

fn parse_request_line(line: &str) -> Option<Request> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    Some(Request {
        method,
        path: percent_decode(path),
        params: parse_query(query),
        body: Vec::new(),
    })
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&value[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// -- routing ---------------------------------------------------------------

fn route(request: &Request, server: &Server) -> Result<Response> {
    // POST is only ever accepted on /t/<id>/<action> edit routes, and
    // only when edit mode is on. Everything else stays GET/HEAD.
    if request.method == "POST" {
        if !server.edit {
            return Ok(Response::html(
                405,
                error_page(
                    "405 - method not allowed",
                    "This server is read-only. Restart with `ti serve --edit` to mutate tickets.",
                ),
            ));
        }
        return edit::route_post(request, server);
    }
    if request.method != "GET" && request.method != "HEAD" {
        return Ok(Response::html(
            405,
            error_page("405 - method not allowed", "This server only answers GET."),
        ));
    }

    match request.path.as_str() {
        "/" => tickets::list_response(request, server),
        "/kanban" => kanban::response(request, server),
        "/tickets.json" => tickets::json_response(request),
        "/favicon.ico" => Ok(Response::empty(204)),
        path => {
            // Static assets for the flow view (embedded at compile time).
            if let Some(asset) = path.strip_prefix("/assets/") {
                return Ok(serve_asset(asset));
            }
            if let Some(reference) = path.strip_prefix("/t/").filter(|r| !r.is_empty()) {
                // /t/<id>/flow → lifecycle view; everything else → detail.
                if let Some(reference) = reference.strip_suffix("/flow") {
                    return flow::response(request, reference, server);
                }
                return tickets::detail_response(reference, server, request);
            }
            Ok(Response::html(
                404,
                error_page("404 - not found", "No page at that address."),
            ))
        }
    }
}

/// Per-request context shared by the pages.
struct Page {
    repo: String,
    current_user: String,
    nicks: NickMap,
    now: OffsetDateTime,
    /// Whether inline edit forms should be rendered.
    edit: bool,
    /// CSRF token echoed into every edit form's hidden field.
    csrf: String,
}

impl Page {
    fn new(store: &TicketStore, server: &Server) -> Result<Self> {
        Ok(Self {
            repo: repo_name(),
            current_user: store.email().to_string(),
            nicks: render::build_nick_map(&store.list_users().unwrap_or_default()),
            now: OffsetDateTime::now_utc(),
            edit: server.edit,
            csrf: server.csrf.clone(),
        })
    }
}

fn repo_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "tickets".to_string())
}

// -- shared chrome ---------------------------------------------------------

/// Carries the active narrowing through the search form, which would
/// otherwise drop it on submit.
fn hidden_input(name: &str, value: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
        escape(name),
        escape(value)
    )
}

/// One active filter, linking to itself removed.
fn filter_chip(label: &str, href: &str) -> String {
    format!(
        "<a class=\"chip\" href=\"{}\">{} \u{d7}</a>",
        escape(href),
        escape(label)
    )
}

/// Stable per-tag colour bucket, mirroring the TUI's tag colouring.
fn tag_hue(tag: &str) -> usize {
    tag.bytes().fold(0usize, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as usize)
    }) % 8
}

fn error_page(title: &str, detail: &str) -> String {
    document(
        title,
        &format!(
            "<header class=\"detail\"><a class=\"back\" href=\"/\">\u{2190} all tickets</a>\
             <h1>{}</h1></header><pre class=\"prose\">{}</pre>",
            escape(title),
            escape(detail)
        ),
    )
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{}</title><style>{STYLE}</style></head><body><main>{body}</main>{KANBAN_SCRIPT}</body></html>\n",
        escape(title)
    )
}

const STYLE: &str = "\
:root{color-scheme:light dark;--bg:#fff;--fg:#1c1c1e;--dim:#6b7280;--line:#e5e7eb;\
--accent:#2563eb;--chip:#f3f4f6;--hover:#f9fafb}\
@media(prefers-color-scheme:dark){:root{--bg:#111317;--fg:#e6e8eb;--dim:#8b93a1;--line:#262a31;\
--accent:#7aa2f7;--chip:#1c2027;--hover:#171a20}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--fg);\
font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}\
main{max-width:1100px;margin:0 auto;padding:24px 20px 60px}\
a{color:inherit;text-decoration:none}a:hover{text-decoration:underline}\
header{display:flex;flex-wrap:wrap;gap:12px;align-items:center;\
padding-bottom:12px;border-bottom:1px solid var(--line);margin-bottom:16px}\
h1{font-size:18px;margin:0;font-weight:600}\
header nav{display:flex;gap:4px;margin-left:8px}\
nav .view{padding:3px 10px;border-radius:999px;color:var(--dim)}\
nav .view:hover{background:var(--hover);text-decoration:none}\
nav .view.active{background:var(--accent);color:#fff}\
header form{margin-left:auto}\
input[type=search]{font:inherit;padding:5px 10px;border:1px solid var(--line);\
border-radius:6px;background:var(--bg);color:var(--fg);min-width:200px}\
.filters{display:flex;gap:6px;flex-wrap:wrap;margin:-4px 0 14px}\
.chip{background:var(--chip);color:var(--dim);border-radius:999px;padding:2px 10px;font-size:12px}\
table{width:100%;border-collapse:collapse}\
th{text-align:left;font-weight:600;color:var(--dim);font-size:12px;\
text-transform:uppercase;letter-spacing:.04em;padding:6px 8px;border-bottom:1px solid var(--line)}\
th a{color:inherit}\
td{padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}\
tbody tr:hover{background:var(--hover)}\
td.id a,td.age{color:var(--dim)}\
td.prio{color:#a855f7}td.age,td.prio,td.id{white-space:nowrap}\
td.title a{font-weight:500}\
tr.closed td.title a{color:var(--dim);text-decoration:line-through}\
td.who{color:var(--dim);white-space:nowrap}td.who.mine{color:#d97706;font-weight:600}\
.children{color:var(--dim)}.children-toggle{background:none;border:none;color:var(--dim);font:inherit;font-size:12px;cursor:pointer;padding:0 2px}.children-toggle:hover{color:var(--accent)}tr.sub td.title{padding-left:calc(10px * var(--depth, 1))}tr.sub{background:var(--hover)}.parent{color:var(--dim);font-size:12px;margin-right:4px}\
.tag{font-size:12px;border-radius:4px;padding:1px 6px;background:var(--chip);white-space:nowrap}\
.tag-0{color:#2563eb}.tag-1{color:#0891b2}.tag-2{color:#16a34a}.tag-3{color:#ca8a04}\
.tag-4{color:#c026d3}.tag-5{color:#0ea5e9}.tag-6{color:#65a30d}.tag-7{color:#e11d48}\
.badge{font-size:12px;border-radius:4px;padding:1px 6px;background:var(--chip)}\
.state-in-progress{color:#d97706}.state-blocked{color:#dc2626}.state-review{color:#2563eb}\
.state-resolved{color:#16a34a}.state-wontfix,.state-duplicate,.state-invalid{color:var(--dim)}\
.count,.empty{color:var(--dim);margin-top:16px}\
header.detail{display:block}.back{color:var(--dim);font-size:12px}\
.subtitle{color:var(--dim);margin:6px 0 0}\
.fields{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:10px;margin:0 0 20px}\
dt{color:var(--dim);font-size:12px;text-transform:uppercase;letter-spacing:.04em}\
dd{margin:2px 0 0}\
h2{font-size:13px;text-transform:uppercase;letter-spacing:.04em;color:var(--dim);margin:24px 0 8px}\
.prose{white-space:pre-wrap;word-wrap:break-word;font:inherit;margin:0;\
background:var(--chip);border-radius:6px;padding:12px}\
.comment{margin-bottom:12px}.byline{color:var(--dim);font-size:12px;margin:0 0 4px}nav.modes{display:flex;gap:4px;margin-left:4px;padding-left:8px;border-left:1px solid var(--line)}.kanban{display:flex;gap:12px;overflow-x:auto;padding-bottom:12px}.kanban-col{flex:0 0 240px;background:var(--chip);border-radius:8px;padding:8px;display:flex;flex-direction:column;min-height:60px}.kanban-col h3{font-size:12px;text-transform:uppercase;letter-spacing:.04em;color:var(--dim);margin:0 0 8px;padding:2px 4px}.kanban-col h3 .n{float:right;font-weight:400}.kanban-cards{display:flex;flex-direction:column;gap:6px}.kcard{background:var(--bg);border:1px solid var(--line);border-radius:6px;padding:8px;display:block}.kcard:hover{border-color:var(--accent);text-decoration:none}.kcard .kt{font-weight:500;font-size:13px;line-height:1.3;margin-bottom:4px}.kcard .km{display:flex;gap:6px;flex-wrap:wrap;align-items:center;font-size:11px;color:var(--dim)}.kcard .kid{font-family:inherit;color:var(--dim)}.kcard .kp{color:#a855f7}.kcard .kpar{color:var(--dim)}.kcard .ka{color:#d97706}.kcard .kp{color:var(--dim)}.kcard .kc{color:var(--dim)}.kcard-sub{border-left:3px solid var(--accent)}.kgroup{display:flex;flex-direction:column;gap:6px}.kcard-parent{border-left:3px solid var(--accent)}.ktoggle{display:flex;align-items:center;gap:4px;background:none;border:none;color:var(--dim);font:inherit;font-size:11px;cursor:pointer;padding:2px 4px;border-radius:4px}.ktoggle:hover{background:var(--hover)}.ktoggle .kcount{background:var(--chip);border-radius:999px;padding:0 6px}.kchildren{display:flex;flex-direction:column;gap:4px;padding-left:12px;border-left:2px solid var(--line);margin-left:8px}.kchildren .kcard{font-size:12px;opacity:.9}.flow-empty{color:var(--dim);margin-top:16px}\
.detail-nav{display:flex;gap:4px;margin-top:10px}\
.detail-nav a{padding:3px 10px;border-radius:999px;color:var(--dim);background:var(--chip)}\
.detail-nav a:hover{background:var(--hover);text-decoration:none}\
.detail-nav a.active{background:var(--accent);color:#fff}\
.edit-error{background:#fef2f2;color:#b91c1c;border:1px solid #fecaca;border-radius:6px;\
padding:8px 10px;margin:0 0 16px}@media(prefers-color-scheme:dark){.edit-error{background:#2a1212;\
color:#fca5a5;border-color:#5b1f1f}}\
section.edit{margin-top:24px}\
.edit-form{display:flex;flex-wrap:wrap;gap:8px;align-items:end;margin:0 0 10px;\
padding:10px;background:var(--chip);border-radius:6px}\
.edit-form label{display:flex;flex-direction:column;gap:3px;font-size:12px;color:var(--dim);\
text-transform:uppercase;letter-spacing:.04em;flex:1 1 200px}\
.edit-form input,.edit-form select,.edit-form textarea{font:inherit;padding:5px 8px;\
border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg);\
font-size:13px;text-transform:none;letter-spacing:0}\
.edit-form textarea{resize:vertical;min-height:3em}\
.edit-form button{font:inherit;padding:5px 14px;border:1px solid var(--accent);\
background:var(--accent);color:#fff;border-radius:6px;cursor:pointer}\
.edit-form button:hover{filter:brightness(1.08)}\
.edit-tag-remove{flex:0 0 auto;align-items:center}\
.edit-tag-remove span{margin-right:4px}\
.edit-comment{flex-direction:column;align-items:stretch}\
.edit-comment label{flex:1 1 auto}";

const KANBAN_SCRIPT: &str = "<script>\
function kanbanToggle(b){{var g=b.closest('.kgroup');var c=g.querySelector('.kchildren');\
var x=b.getAttribute('aria-expanded')==='true';\
c.style.display=x?'none':'flex';b.setAttribute('aria-expanded',x?'false':'true');\
b.innerHTML=(x?'\\u25B6':'\\u25BC')+'<span class=\"kcount\">'+c.children.length+'</span>';}}\
function listToggle(b){{var tr=b.closest('tr');var pid=tr.getAttribute('data-id');\
var x=b.getAttribute('aria-expanded')==='true';\
var rows=tr.closest('tbody').querySelectorAll('tr[data-parent=\"'+pid+'\"]');\
for(var i=0;i<rows.length;i++){{rows[i].style.display=x?'none':'table-row';}}\
b.setAttribute('aria-expanded',x?'false':'true');\
b.textContent=(x?'[+':'[-')+rows.length+']';}}\
</script>";

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn flatten(value: &str) -> String {
    value.replace(['\n', '\r', '\t'], " ")
}

// -- vendor assets ---------------------------------------------------------

/// Embedded vendor assets for the flow view (React, ReactDOM,
/// @xyflow/react UMD bundles + CSS).  Served at `/assets/<name>` so
/// the flow page has no CDN dependencies.
static REACT_JS: &str = include_str!(env!("TICGIT_VENDOR_REACT_MIN_JS"));
static REACT_DOM_JS: &str = include_str!(env!("TICGIT_VENDOR_REACT_DOM_MIN_JS"));
static XYFLOW_JS: &str = include_str!(env!("TICGIT_VENDOR_XYFLOW_MIN_JS"));
static XYFLOW_CSS: &str = include_str!(env!("TICGIT_VENDOR_XYFLOW_CSS"));
static JSX_SHIM_JS: &str = include_str!(env!("TICGIT_VENDOR_JSX_RUNTIME_SHIM_JS"));

fn serve_asset(name: &str) -> Response {
    match name {
        "react.min.js" => Response::new(200, "text/javascript; charset=utf-8", REACT_JS.as_bytes().to_vec()),
        "react-dom.min.js" => Response::new(200, "text/javascript; charset=utf-8", REACT_DOM_JS.as_bytes().to_vec()),
        "xyflow.min.js" => Response::new(200, "text/javascript; charset=utf-8", XYFLOW_JS.as_bytes().to_vec()),
        "xyflow.css" => Response::new(200, "text/css; charset=utf-8", XYFLOW_CSS.as_bytes().to_vec()),
        "jsx-runtime-shim.js" => Response::new(200, "text/javascript; charset=utf-8", JSX_SHIM_JS.as_bytes().to_vec()),
        _ => Response::html(404, error_page("404 - not found", "No such asset.")),
    }
}

// -- responses -------------------------------------------------------------

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    /// Set on 3xx redirects; written as the `Location` header.
    location: Option<String>,
}

impl Response {
    fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
            location: None,
        }
    }

    fn html(status: u16, body: String) -> Self {
        Self::new(status, "text/html; charset=utf-8", body.into_bytes())
    }

    fn empty(status: u16) -> Self {
        Self::new(status, "text/plain; charset=utf-8", Vec::new())
    }

    /// `303 See Other` — used by every successful POST so a refresh
    /// doesn't resubmit. The body is a tiny HTML link for non-browser
    /// clients; browsers follow the `Location` header.
    fn redirect(location: &str) -> Self {
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>See Other</title>\
             <a href=\"{}\">continue</a>",
            escape(location)
        );
        let mut resp = Self::html(303, body);
        resp.location = Some(location.to_string());
        resp
    }

    fn write_to(&self, stream: &mut TcpStream) -> Result<()> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n",
            self.status,
            reason(self.status),
            self.content_type,
            self.body.len()
        );
        if let Some(location) = &self.location {
            head.push_str(&format!("Location: {}\r\n", location));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()?;
        Ok(())
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        303 => "See Other",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn open_browser(url: &str) {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let _ = std::process::Command::new(program)
        .args(args)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target: &str) -> Request {
        parse_request_line(&format!("GET {target} HTTP/1.1\r\n")).unwrap()
    }

    fn readonly_server() -> Server {
        Server {
            edit: false,
            csrf: "test-csrf".to_string(),
        }
    }

    fn edit_server() -> Server {
        Server {
            edit: true,
            csrf: "test-csrf".to_string(),
        }
    }

    #[test]
    fn parses_request_line_into_path_and_params() {
        let req = request("/?status=closed&tag=bug&tag=ui");
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/");
        assert_eq!(req.param("status"), Some("closed"));
        assert_eq!(req.param_values("tag"), vec!["bug", "ui"]);
    }

    #[test]
    fn percent_decoding_handles_escapes_and_plus() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn percent_encode_round_trips() {
        let value = "tag with spaces & ?=#";
        assert_eq!(percent_decode(&percent_encode(value)), value);
    }

    #[test]
    fn form_body_is_parsed_into_fields() {
        let mut req = parse_request_line("POST /t/abc/comment HTTP/1.1\r\n").unwrap();
        req.body = b"body=hello+world&csrf=test-csrf".to_vec();
        assert_eq!(req.form_field("body"), Some("hello world".to_string()));
        assert_eq!(req.form_field("csrf"), Some("test-csrf".to_string()));
        assert_eq!(req.form_field("missing"), None);
    }

    #[test]
    fn unknown_paths_are_404_and_non_get_is_405() {
        let server = readonly_server();
        let response = route(&request("/nope"), &server).unwrap();
        assert_eq!(response.status, 404);

        let post = parse_request_line("POST / HTTP/1.1\r\n").unwrap();
        // Read-only server rejects POST everywhere.
        assert_eq!(route(&post, &server).unwrap().status, 405);
    }

    #[test]
    fn post_to_non_edit_path_is_405_even_in_edit_mode() {
        let server = edit_server();
        let post = parse_request_line("POST / HTTP/1.1\r\n").unwrap();
        assert_eq!(route(&post, &server).unwrap().status, 405);
    }

    #[test]
    fn redirect_response_carries_location_header() {
        let resp = Response::redirect("/t/abc");
        assert_eq!(resp.status, 303);
        assert_eq!(resp.location.as_deref(), Some("/t/abc"));
    }
}
