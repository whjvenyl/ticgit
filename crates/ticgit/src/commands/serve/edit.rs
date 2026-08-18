//! POST handlers for `ti serve --edit`.
//!
//! Each handler validates the CSRF token, runs the same `TicketStore`
//! mutation the matching CLI command runs, then redirects back to the
//! ticket detail page (PRG). On error it redirects back with
//! `?error=<message>` so the detail page can render a banner.

use anyhow::Result;
use ticgit_lib::TicketLifecycle;

use super::{percent_encode, Request, Response, Server};
use crate::commands::open_store;

/// Route a POST. The path is expected to be `/t/<id>/<action>`; anything
/// else is 404 (or 405 for a bare `/t/<id>`).
pub(super) fn route_post(request: &Request, server: &Server) -> Result<Response> {
    let path = request.path.as_str();
    let rest = match path.strip_prefix("/t/") {
        Some(rest) if !rest.is_empty() => rest,
        _ => return Ok(super::Response::html(
            405,
            super::error_page("405 - method not allowed", "POST is only allowed on /t/<id>/<action>."),
        )),
    };
    let (reference, action) = match rest.rsplit_once('/') {
        Some((reference, action)) => (reference, action),
        None => return Ok(super::Response::html(
            405,
            super::error_page("405 - method not allowed", "POST needs an action, e.g. /t/<id>/comment."),
        )),
    };

    // CSRF: every edit form embeds the server token; reject anything else.
    if !check_csrf(request, server) {
        return Ok(redirect_with_error(reference, "missing or invalid CSRF token"));
    }

    match action {
        "comment" => handle_comment(request, reference),
        "state" => handle_state(request, reference),
        "assign" => handle_assign(request, reference),
        "priority" => handle_priority(request, reference),
        "tags" => handle_tags(request, reference),
        other => Ok(redirect_with_error(
            reference,
            &format!("unknown edit action `{other}`"),
        )),
    }
}

fn check_csrf(request: &Request, server: &Server) -> bool {
    request.form_field("csrf").as_deref() == Some(server.csrf.as_str())
}

/// `303 See Other` to `/t/<id>?error=<message>`.
fn redirect_with_error(reference: &str, message: &str) -> Response {
    let location = format!("/t/{}?error={}", reference, percent_encode(message));
    Response::redirect(&location)
}

/// `303 See Other` to `/t/<id>` (clean).
fn redirect_to_detail(reference: &str) -> Response {
    Response::redirect(&format!("/t/{reference}"))
}

// -- handlers --------------------------------------------------------------

fn handle_comment(request: &Request, reference: &str) -> Result<Response> {
    let body = match request.form_field("body") {
        Some(b) => b,
        None => return Ok(redirect_with_error(reference, "comment body cannot be empty")),
    };
    let store = open_store()?;
    let id = store.resolve_id(reference)?;
    store.add_comment(&id, &body)?;
    Ok(redirect_to_detail(reference))
}

fn handle_state(request: &Request, reference: &str) -> Result<Response> {
    let spec = match request.form_field("state") {
        Some(s) => s,
        None => return Ok(redirect_with_error(reference, "no state provided")),
    };
    let lifecycle = match TicketLifecycle::parse(&spec) {
        Ok(lc) => lc,
        Err(err) => return Ok(redirect_with_error(reference, &err.to_string())),
    };
    let store = open_store()?;
    let id = store.resolve_id(reference)?;
    store.set_lifecycle(&id, lifecycle.status, lifecycle.state)?;
    Ok(redirect_to_detail(reference))
}

fn handle_assign(request: &Request, reference: &str) -> Result<Response> {
    let assigned = request.form_field("assigned"); // None or empty clears.
    let store = open_store()?;
    let id = store.resolve_id(reference)?;
    store.set_assigned(&id, assigned.as_deref())?;
    Ok(redirect_to_detail(reference))
}

fn handle_priority(request: &Request, reference: &str) -> Result<Response> {
    let priority = match request.form_field("priority") {
        Some(raw) => match raw.parse::<i64>() {
            Ok(n) => Some(n),
            Err(_) => return Ok(redirect_with_error(reference, "priority must be a number")),
        },
        None => None,
    };
    let store = open_store()?;
    let id = store.resolve_id(reference)?;
    store.set_priority(&id, priority)?;
    Ok(redirect_to_detail(reference))
}

fn handle_tags(request: &Request, reference: &str) -> Result<Response> {
    let store = open_store()?;
    let id = store.resolve_id(reference)?;
    if let Some(tag) = request.form_field("add") {
        store.add_tag(&id, &tag)?;
    }
    if let Some(tag) = request.form_field("remove") {
        store.remove_tag(&id, &tag)?;
    }
    Ok(redirect_to_detail(reference))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{parse_request_line, Server};

    fn server() -> Server {
        Server {
            edit: true,
            csrf: "test-csrf".to_string(),
        }
    }

    fn post(target: &str, body: &str) -> Request {
        let mut req = parse_request_line(&format!("POST {target} HTTP/1.1\r\n")).unwrap();
        req.body = body.as_bytes().to_vec();
        req
    }

    #[test]
    fn csrf_mismatch_is_rejected_with_error_redirect() {
        let server = server();
        let req = post("/t/abc/comment", "body=hi&csrf=wrong");
        let resp = route_post(&req, &server).unwrap();
        assert_eq!(resp.status, 303);
        assert!(resp.location.as_deref().unwrap().contains("error="));
    }

    #[test]
    fn unknown_action_redirects_with_error() {
        let server = server();
        let req = post("/t/abc/frobnicate", "csrf=test-csrf");
        let resp = route_post(&req, &server).unwrap();
        assert_eq!(resp.status, 303);
        assert!(resp.location.as_deref().unwrap().contains("unknown%20edit%20action"));
    }

    #[test]
    fn bare_ticket_post_is_405() {
        let server = server();
        let req = post("/t/abc", "csrf=test-csrf");
        let resp = route_post(&req, &server).unwrap();
        assert_eq!(resp.status, 405);
    }

    #[test]
    fn check_csrf_matches_server_token() {
        let server = server();
        let req = post("/t/abc/comment", "csrf=test-csrf");
        assert!(check_csrf(&req, &server));
        let req = post("/t/abc/comment", "csrf=nope");
        assert!(!check_csrf(&req, &server));
        let req = post("/t/abc/comment", "");
        assert!(!check_csrf(&req, &server));
    }
}
