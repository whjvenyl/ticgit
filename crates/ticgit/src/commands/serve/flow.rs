//! Per-ticket lifecycle view for `ti serve`.
//!
//! Renders a single ticket's journey through its states as a directed
//! graph (one node per unique state, edges = transitions).  Loops —
//! a ticket that bounces `review → in-progress → review` — draw as
//! back-edges so the cyclical nature is immediately visible, unlike
//! a flat forward timeline.
//!
//! Below the graph, a timeline lists every recorded change grouped
//! by the state-window it happened in, mirroring the GitHub Actions
//! pattern of a job graph on top and a step log beneath.
//!
//! Data comes from the git-meta metadata log (the same source as
//! `ti history`), reconstructed into a chronological sequence of
//! state visits with their intervening steps.

use std::collections::HashMap;

use anyhow::Result;
use ticgit_lib::{Ticket, TicketState};
use time::{Duration, OffsetDateTime};

use super::{escape, Page, Request, Response};
use crate::commands::history::{db_path_for, query_history, HistoryEntry};
use crate::commands::{open_store, SessionGitDir};
use crate::render;
use crate::timefmt::relative_time;

// -- response ---------------------------------------------------------------

pub(super) fn response(_request: &Request, reference: &str) -> Result<Response> {
    let store = open_store()?;
    let id = match store.resolve_id(reference) {
        Ok(id) => id,
        Err(err) => {
            return Ok(Response::html(
                404,
                super::error_page("404 - no such ticket", &err.to_string()),
            ))
        }
    };
    let ticket = store.load(&id)?;
    let page = Page::new(&store)?;

    // Try to load history; fall back to current-state-only if the
    // git-meta sqlite DB isn't available (e.g. fresh clone).
    let git_dir = store.session().repo_git_dir();
    let history = db_path_for(&git_dir)
        .and_then(|path| query_history(&path, &id.to_string(), None))
        .unwrap_or_default();

    let lifecycle = build_lifecycle(&ticket, &history, page.now);
    Ok(Response::html(200, flow_page(&page, &ticket, &lifecycle)))
}

// -- lifecycle reconstruction ----------------------------------------------

/// One contiguous window the ticket spent in a single state.
struct StateVisit {
    state: TicketState,
    entered_at: OffsetDateTime,
    entered_by: String,
    /// 0-based index in the chronological visit list.
    seq: usize,
    /// Time until the next transition (or until "now" for the last).
    duration: Duration,
    /// Non-state changes that happened during this window.
    steps: Vec<HistoryEntry>,
}

struct Lifecycle {
    visits: Vec<StateVisit>,
}

fn build_lifecycle(ticket: &Ticket, history: &[HistoryEntry], now: OffsetDateTime) -> Lifecycle {
    if history.is_empty() {
        return Lifecycle {
            visits: vec![StateVisit {
                state: ticket.state,
                entered_at: ticket.created_at,
                entered_by: ticket.created_by.clone(),
                seq: 0,
                duration: now - ticket.created_at,
                steps: Vec::new(),
            }],
        };
    }

    // Sort chronologically (query_history returns DESC, but sorting
    // is more robust than relying on that).
    let mut entries: Vec<&HistoryEntry> = history.iter().collect();
    entries.sort_by_key(|e| e.at.unix_timestamp());

    // State transitions are the `state` field sets.  `status` is
    // redundant (always set alongside `state` by `set_lifecycle`).
    let transitions: Vec<&HistoryEntry> = entries
        .iter()
        .copied()
        .filter(|e| e.field == "state")
        .collect();

    if transitions.is_empty() {
        // No state changes recorded — single visit with current state.
        let steps: Vec<HistoryEntry> = entries
            .iter()
            .copied()
            .filter(|e| e.field != "status")
            .cloned()
            .collect();
        return Lifecycle {
            visits: vec![StateVisit {
                state: ticket.state,
                entered_at: ticket.created_at,
                entered_by: ticket.created_by.clone(),
                seq: 0,
                duration: now - ticket.created_at,
                steps,
            }],
        };
    }

    let mut visits: Vec<StateVisit> = Vec::new();
    for (i, trans) in transitions.iter().enumerate() {
        let state = TicketState::parse(&trans.value).unwrap_or(TicketState::New);
        let entered_at = trans.at;
        let entered_by = trans.email.clone();
        let next_at = transitions.get(i + 1).map(|t| t.at).unwrap_or(now);
        let duration = next_at - entered_at;

        // Bucket non-state entries into the window [entered_at, next_at).
        let steps: Vec<HistoryEntry> = entries
            .iter()
            .copied()
            .filter(|e| {
                e.field != "state" && e.field != "status" && e.at >= entered_at && e.at < next_at
            })
            .cloned()
            .collect();

        visits.push(StateVisit {
            state,
            entered_at,
            entered_by,
            seq: i,
            duration,
            steps,
        });
    }

    Lifecycle { visits }
}

// -- rendering --------------------------------------------------------------

fn flow_page(page: &Page, ticket: &Ticket, lifecycle: &Lifecycle) -> String {
    let mut body = String::new();

    // Header — detail-style with a back-link to the ticket.
    body.push_str(&format!(
        "<header class=\"detail\"><a class=\"back\" href=\"/\">\u{2190} all tickets</a>\
         <h1>{}</h1><p class=\"subtitle\"><span class=\"badge state-{}\">{}</span> \
         <code>{}</code> \u{b7} opened {} ago by {}</p>\
         <nav class=\"detail-nav\"><a href=\"/t/{}\">Ticket</a>\
         <a href=\"/t/{}/flow\" class=\"active\">Lifecycle</a></nav></header>",
        escape(&ticket.title),
        escape(ticket.state.as_str()),
        escape(ticket.state.as_str()),
        escape(&ticket.short_id()),
        escape(&relative_time(ticket.created_at, page.now)),
        escape(&render::display_name(&ticket.created_by, Some(&page.nicks))),
        escape(&ticket.short_id()),
        escape(&ticket.short_id()),
    ));

    // -- graph -----------------------------------------------------------
    let nodes_json = build_nodes_json(lifecycle);
    let edges_json = build_edges_json(lifecycle);

    body.push_str("<div class=\"flow-wrap\" id=\"flow\"></div>");

    // -- timeline --------------------------------------------------------
    body.push_str("<section class=\"lifecycle-timeline\">");
    body.push_str("<h2>Timeline</h2>");
    for visit in &lifecycle.visits {
        body.push_str(&render_visit(visit, page));
    }
    body.push_str("</section>");

    flow_document(
        &format!("{} \u{b7} lifecycle", ticket.short_id()),
        &body,
        &nodes_json,
        &edges_json,
    )
}

/// Build React Flow nodes — one per unique state, positioned left-to-right
/// by first-visit order.
fn build_nodes_json(lifecycle: &Lifecycle) -> String {
    // Unique states in order of first appearance.
    let mut unique: Vec<TicketState> = Vec::new();
    for visit in &lifecycle.visits {
        if !unique.contains(&visit.state) {
            unique.push(visit.state);
        }
    }

    // Visit count per state.
    let mut counts: HashMap<TicketState, usize> = HashMap::new();
    for visit in &lifecycle.visits {
        *counts.entry(visit.state).or_default() += 1;
    }

    // First visit per state (for the "first seen" label).
    let mut first: HashMap<TicketState, &StateVisit> = HashMap::new();
    for visit in &lifecycle.visits {
        first.entry(visit.state).or_insert(visit);
    }

    // The last visit's state is "current".
    let current_state = lifecycle.visits.last().map(|v| v.state);

    const COL_WIDTH: f64 = 220.0;
    const X_OFFSET: f64 = 40.0;
    const Y_OFFSET: f64 = 40.0;

    let nodes: Vec<String> = unique
        .iter()
        .enumerate()
        .map(|(col, &state)| {
            let x = X_OFFSET + col as f64 * COL_WIDTH;
            let y = Y_OFFSET;
            let count = counts.get(&state).copied().unwrap_or(1);
            let first_visit = first.get(&state).copied();
            let is_current = current_state == Some(state);
            let is_closed = state.status() == ticgit_lib::TicketStatus::Closed;

            let icon = if is_closed {
                match state {
                    TicketState::Resolved => " \u{2713}",
                    _ => " \u{2717}",
                }
            } else {
                ""
            };

            let count_badge = if count > 1 {
                format!("<span class=\"lnode-count\">\u{00d7}{}</span>", count)
            } else {
                String::new()
            };

            let meta = if let Some(fv) = first_visit {
                if is_current {
                    format!(
                        "<div class=\"lnode-meta\">current \u{b7} {}</div>",
                        escape(&duration_str(fv.duration))
                    )
                } else {
                    format!(
                        "<div class=\"lnode-meta\">{}</div>",
                        escape(&duration_str(fv.duration))
                    )
                }
            } else {
                String::new()
            };

            let current_class = if is_current { " current" } else { "" };

            let label = format!(
                "<div class=\"lnode{}\"><span class=\"lnode-state state-{}\">{}{}</span>{}{}</div>",
                current_class,
                escape(state.as_str()),
                escape(state.as_str()),
                icon,
                count_badge,
                meta,
            );

            format!(
                "{{\"id\":\"{}\",\"data\":{{\"label\":{}}},\"position\":{{\"x\":{},\"y\":{}}},\"type\":\"default\",\"draggable\":false}}",
                escape(state.as_str()),
                serde_json::to_string(&label).unwrap_or_else(|_| "\"\"".to_string()),
                x,
                y,
            )
        })
        .collect();

    nodes.join(",")
}

/// Build React Flow edges — one per transition between consecutive visits.
/// Back-edges (revisiting an earlier state) are drawn with a dashed style
/// so loops are obvious.
fn build_edges_json(lifecycle: &Lifecycle) -> String {
    let mut edges: Vec<String> = Vec::new();

    for i in 0..lifecycle.visits.len().saturating_sub(1) {
        let from = &lifecycle.visits[i];
        let to = &lifecycle.visits[i + 1];
        let transition_num = i + 1;
        let dur = duration_str(from.duration);

        // A back-edge is one that goes to a state whose first visit
        // was earlier than the source's first visit — i.e. the target
        // node is to the left of the source node.
        let from_first = lifecycle
            .visits
            .iter()
            .position(|v| v.state == from.state)
            .unwrap_or(0);
        let to_first = lifecycle
            .visits
            .iter()
            .position(|v| v.state == to.state)
            .unwrap_or(0);
        let is_back_edge = to_first <= from_first && from.state != to.state;

        let label = format!("#{} \u{b7} {}", transition_num, dur);

        let style = if is_back_edge {
            ",\"style\":{\"strokeDasharray\":\"6 4\",\"stroke\":\"#dc2626\"}"
        } else {
            ""
        };

        edges.push(format!(
            "{{\"id\":\"e-{}\",\"source\":\"{}\",\"target\":\"{}\",\"type\":\"smoothstep\",\"animated\":false,\"label\":{}{}}}",
            transition_num,
            escape(from.state.as_str()),
            escape(to.state.as_str()),
            serde_json::to_string(&label).unwrap_or_else(|_| "\"\"".to_string()),
            style,
        ));
    }

    edges.join(",")
}

/// Render one visit as a timeline section with its steps.
fn render_visit(visit: &StateVisit, page: &Page) -> String {
    let who = render::display_name(&visit.entered_by, Some(&page.nicks));
    let dur = duration_str(visit.duration);
    let when = relative_time(visit.entered_at, page.now);

    let mut out = format!(
        "<div class=\"lvisit\">\
         <div class=\"lvisit-header\">\
         <span class=\"lseq\">#{}</span>\
         <span class=\"badge state-{}\">{}</span>\
         <span class=\"ldur\">{}</span>\
         <span class=\"lwhen\">{} ago</span>\
         <span class=\"lwho\">\u{b7} {}</span>\
         </div>",
        visit.seq,
        escape(visit.state.as_str()),
        escape(visit.state.as_str()),
        escape(&dur),
        escape(&when),
        escape(&who),
    );

    if visit.steps.is_empty() {
        out.push_str("<div class=\"lsteps-empty\">No changes during this state.</div>");
    } else {
        out.push_str("<ul class=\"lsteps\">");
        for step in &visit.steps {
            out.push_str(&render_step(step, page));
        }
        out.push_str("</ul>");
    }

    out.push_str("</div>");
    out
}

/// Render a single history entry as a timeline step.
fn render_step(entry: &HistoryEntry, page: &Page) -> String {
    let time = format!(
        "{:02}:{:02}",
        entry.at.hour(),
        entry.at.minute()
    );
    let date = format!(
        "{:04}-{:02}-{:02}",
        entry.at.year(),
        u8::from(entry.at.month()),
        entry.at.day()
    );

    let op = match entry.operation.as_str() {
        "set" => "set",
        "remove" => "removed",
        "set_add" => "added",
        "set_remove" | "set_rm" => "removed",
        "push" => "added",
        other => other,
    };

    let value = if entry.value.len() > 60 {
        format!("{}...", &entry.value[..57])
    } else {
        entry.value.clone()
    };

    let who = render::display_name(&entry.email, Some(&page.nicks));

    format!(
        "<li class=\"lstep\"><time datetime=\"{} {}\">{} {}</time> \
         <span class=\"lstep-op\">{}</span> <code>{}</code> \
         <span class=\"lstep-val\">{}</span> \
         <span class=\"lstep-who\">\u{b7} {}</span></li>",
        escape(&date),
        escape(&time),
        escape(&date),
        escape(&time),
        escape(op),
        escape(&entry.field),
        escape(&value),
        escape(&who),
    )
}

// -- helpers ----------------------------------------------------------------

/// Format a `Duration` as a compact relative string (e.g. "3d", "2h", "5m").
fn duration_str(d: Duration) -> String {
    let seconds = d.whole_seconds().max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 60 * 60 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 60 * 60 * 24 {
        return format!("{}h", seconds / (60 * 60));
    }
    if seconds < 60 * 60 * 24 * 30 {
        return format!("{}d", seconds / (60 * 60 * 24));
    }
    if seconds < 60 * 60 * 24 * 365 {
        return format!("{}mo", seconds / (60 * 60 * 24 * 30));
    }
    format!("{}y", seconds / (60 * 60 * 24 * 365))
}

// -- HTML document ----------------------------------------------------------

/// Like `document` but loads the local UMD vendor bundles (React,
/// ReactDOM, @xyflow/react) and the flow CSS, then bootstraps the
/// React Flow component.  No CDN dependencies.
fn flow_document(title: &str, body: &str, nodes: &str, edges: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title>\
         <style>{STYLE}{LIFECYCLE_STYLE}</style>\
         <link rel=\"stylesheet\" href=\"/assets/xyflow.css\">\
         </head>\
         <body><main>{body}</main>\
         <script src=\"/assets/react.min.js\"></script>\
         <script src=\"/assets/react-dom.min.js\"></script>\
         <script src=\"/assets/jsx-runtime-shim.js\"></script>\
         <script src=\"/assets/xyflow.min.js\"></script>\
         <script>{SCRIPT}</script></body></html>\n",
        title = escape(title),
        STYLE = super::STYLE,
        LIFECYCLE_STYLE = LIFECYCLE_STYLE,
        SCRIPT = bootstrap_script(nodes, edges),
    )
}

fn bootstrap_script(nodes: &str, edges: &str) -> String {
    format!(
        r#"(function() {{
var React = window.React;
var ReactDOM = window.ReactDOM;
var ReactFlow = window.ReactFlow;

var nodes = [{nodes}].map(function(n) {{
  return Object.assign({{}}, n, {{ style: {{ width: 180 }} }});
}});
var edges = [{edges}].map(function(e) {{
  return Object.assign({{}}, e, {{
    markerEnd: {{ type: ReactFlow.MarkerType.ArrowClosed }},
    labelStyle: {{ fontSize: 11, fontFamily: 'ui-monospace, monospace' }},
    labelBgStyle: {{ fill: 'var(--chip)' }},
    labelBgPadding: [4, 2],
    labelBgBorderRadius: 4,
  }});
}});

// Detect dark mode and add the class React Flow expects.
var flowEl = document.getElementById('flow');
var isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;

function Flow() {{
  return React.createElement(ReactFlow.ReactFlow, {{
    nodes: nodes,
    edges: edges,
    fitView: true,
    fitViewOptions: {{ padding: 0.3 }},
    nodesDraggable: true,
    nodesConnectable: false,
    elementsSelectable: false,
    panOnDrag: true,
    zoomOnScroll: true,
    minZoom: 0.2,
    maxZoom: 2,
    className: isDark ? 'dark' : '',
  }},
    React.createElement(ReactFlow.Background, {{ variant: 'dots', gap: 16, size: 1 }}),
    React.createElement(ReactFlow.Controls, {{ showInteractive: false }})
  );
}}

ReactDOM.createRoot(flowEl).render(React.createElement(Flow));
}})();
"#
    )
}

const LIFECYCLE_STYLE: &str = "\
.flow-wrap{width:100%;height:380px;border:1px solid var(--line);border-radius:8px;overflow:hidden;margin-bottom:24px}\
.lnode{width:160px;padding:8px;text-align:center}\
.lnode-state{display:inline-block;font-size:12px;font-weight:600;border-radius:4px;padding:2px 8px;background:var(--chip)}\
.lnode-count{font-size:11px;color:var(--dim);margin-left:4px}\
.lnode-meta{font-size:10px;color:var(--dim);margin-top:4px}\
.lnode.current .lnode-state{box-shadow:0 0 0 2px var(--accent)}\
.lifecycle-timeline{margin-top:8px}\
.lvisit{margin-bottom:12px;border:1px solid var(--line);border-radius:6px;overflow:hidden}\
.lvisit-header{display:flex;align-items:center;gap:8px;padding:8px 12px;background:var(--chip);font-size:13px}\
.lseq{color:var(--dim);font-size:12px;min-width:28px}\
.ldur{color:var(--fg);font-size:12px;font-weight:500}\
.lwhen{color:var(--dim);font-size:12px}\
.lwho{color:var(--dim);font-size:12px}\
.lsteps{list-style:none;margin:0;padding:0}\
.lstep{padding:5px 12px;font-size:12px;border-bottom:1px solid var(--line);display:flex;gap:6px;align-items:baseline;flex-wrap:wrap}\
.lstep:last-child{border-bottom:none}\
.lstep time{color:var(--dim);white-space:nowrap}\
.lstep .lstep-op{color:var(--accent);min-width:48px}\
.lstep code{color:var(--dim)}\
.lstep .lstep-val{color:var(--fg)}\
.lstep .lstep-who{color:var(--dim)}\
.lsteps-empty{padding:8px 12px;color:var(--dim);font-size:12px}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::NickMap;
    use std::collections::{BTreeMap, BTreeSet};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn ticket(id: &str, title: &str, state: TicketState) -> Ticket {
        Ticket {
            id: Uuid::parse_str(id).unwrap(),
            title: title.to_string(),
            description: None,
            spec: None,
            status: state.status(),
            state,
            assigned: None,
            closed_by: None,
            priority: None,
            points: None,
            milestone: None,
            code: None,
            parent: None,
            children: BTreeSet::new(),
            depends_on: BTreeSet::new(),
            blocks: BTreeSet::new(),
            tags: BTreeSet::new(),
            meta: BTreeMap::new(),
            comments: vec![],
            created_at: OffsetDateTime::UNIX_EPOCH,
            created_by: "tester@example.com".into(),
        }
    }

    fn page() -> Page {
        Page {
            repo: "ticgit".to_string(),
            current_user: "tester@example.com".to_string(),
            nicks: NickMap::new(),
            now: OffsetDateTime::UNIX_EPOCH + Duration::days(10),
        }
    }

    fn hist(field: &str, value: &str, op: &str, secs: i64) -> HistoryEntry {
        HistoryEntry {
            field: field.to_string(),
            value: value.to_string(),
            operation: op.to_string(),
            email: "tester@example.com".to_string(),
            at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs),
        }
    }

    #[test]
    fn empty_history_yields_single_visit_with_current_state() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::InProgress,
        );
        let lc = build_lifecycle(&t, &[], page().now);
        assert_eq!(lc.visits.len(), 1);
        assert_eq!(lc.visits[0].state, TicketState::InProgress);
    }

    #[test]
    fn state_transitions_become_visits() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::Review,
        );
        let history = vec![
            hist("state", "new", "set", 0),
            hist("state", "in-progress", "set", 100),
            hist("state", "review", "set", 500),
        ];
        let lc = build_lifecycle(&t, &history, page().now);
        assert_eq!(lc.visits.len(), 3);
        assert_eq!(lc.visits[0].state, TicketState::New);
        assert_eq!(lc.visits[1].state, TicketState::InProgress);
        assert_eq!(lc.visits[2].state, TicketState::Review);
    }

    #[test]
    fn loops_produce_back_edges() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::Review,
        );
        let history = vec![
            hist("state", "new", "set", 0),
            hist("state", "in-progress", "set", 100),
            hist("state", "review", "set", 500),
            hist("state", "in-progress", "set", 800),
            hist("state", "review", "set", 1200),
        ];
        let lc = build_lifecycle(&t, &history, page().now);
        // 5 transitions → 5 visits, 4 edges.
        assert_eq!(lc.visits.len(), 5);
        let edges = build_edges_json(&lc);
        // The edge from review→in-progress (transition #3) should be a
        // back-edge (dashed, red).
        assert!(edges.contains("e-3"));
        assert!(edges.contains("strokeDasharray"));
    }

    #[test]
    fn steps_are_bucketed_into_state_windows() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::InProgress,
        );
        let history = vec![
            hist("state", "new", "set", 0),
            hist("tags", "bug", "set_add", 50),
            hist("state", "in-progress", "set", 100),
            hist("assigned", "alice@example.com", "set", 150),
            hist("comments", "(comment added)", "push", 200),
        ];
        let lc = build_lifecycle(&t, &history, page().now);
        assert_eq!(lc.visits.len(), 2);
        // "new" window [0, 100) → tag added at t=50
        assert_eq!(lc.visits[0].steps.len(), 1);
        assert_eq!(lc.visits[0].steps[0].field, "tags");
        // "in-progress" window [100, now) → assigned + comment
        assert_eq!(lc.visits[1].steps.len(), 2);
    }

    #[test]
    fn flow_page_contains_local_vendor_assets() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::New,
        );
        let lc = build_lifecycle(&t, &[], page().now);
        let html = flow_page(&page(), &t, &lc);
        // Should reference local assets, not CDN.
        assert!(html.contains("/assets/react.min.js"));
        assert!(html.contains("/assets/xyflow.min.js"));
        assert!(html.contains("/assets/xyflow.css"));
        assert!(!html.contains("esm.sh"));
        assert!(html.contains("ReactFlow"));
    }

    #[test]
    fn flow_page_shows_timeline_section() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::New,
        );
        let lc = build_lifecycle(&t, &[], page().now);
        let html = flow_page(&page(), &t, &lc);
        assert!(html.contains("lifecycle-timeline"));
        assert!(html.contains("Timeline"));
    }

    #[test]
    fn flow_page_links_back_to_ticket() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::New,
        );
        let lc = build_lifecycle(&t, &[], page().now);
        let html = flow_page(&page(), &t, &lc);
        assert!(html.contains("href=\"/t/d7f2d8\""));
    }

    #[test]
    fn duration_str_formats_compactly() {
        assert_eq!(duration_str(Duration::seconds(30)), "30s");
        assert_eq!(duration_str(Duration::seconds(90)), "1m");
        assert_eq!(duration_str(Duration::seconds(3600)), "1h");
        assert_eq!(duration_str(Duration::days(3)), "3d");
    }
}
