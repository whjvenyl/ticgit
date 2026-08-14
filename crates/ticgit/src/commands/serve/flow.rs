//! Flow / dependency-graph view for `ti serve`.
//!
//! Renders tickets as nodes and their dependency / parent-child links as
//! edges using [React Flow](https://xyflow.com/) loaded via esm.sh.  The
//! view is read-only: nodes are not draggable, but pan and zoom work.
//!
//! Node positions are computed server-side with a simple layered layout:
//! tickets are grouped by state into columns (like the kanban), and
//! stacked vertically within each column.

use std::collections::HashMap;

use anyhow::Result;
use ticgit_lib::{Ticket, TicketState};
use uuid::Uuid;

use super::{document, escape, flatten, Page, Request, Response};
use super::tickets::{header, ListQuery, View};
use crate::commands::open_store;

pub(super) fn response(request: &Request) -> Result<Response> {
    let store = open_store()?;
    let query = ListQuery::from_request(request);
    let tickets = ticgit_lib::query::apply(store.list()?, &query.filter()?);
    let page = Page::new(&store)?;
    Ok(Response::html(200, flow_page(&page, &query, &tickets)))
}

fn flow_page(page: &Page, query: &ListQuery, tickets: &[Ticket]) -> String {
    let mut body = String::new();
    body.push_str(&header(page, query, View::Flow));

    if tickets.is_empty() {
        body.push_str("<p class=\"flow-empty\">No tickets match this view.</p>");
        return document(&format!("{} flow", page.repo), &body);
    }

    // Build a lookup of which ticket IDs are present.
    let present: std::collections::HashSet<Uuid> = tickets.iter().map(|t| t.id).collect();

    // Compute node positions: group by state into columns, stack vertically.
    let mut columns: HashMap<TicketState, Vec<&Ticket>> = HashMap::new();
    for t in tickets {
        columns.entry(t.state).or_default().push(t);
    }

    const COL_WIDTH: f64 = 260.0;
    const ROW_HEIGHT: f64 = 90.0;
    const X_OFFSET: f64 = 20.0;
    const Y_OFFSET: f64 = 20.0;

    let mut positions: HashMap<Uuid, (f64, f64)> = HashMap::new();
    for (col_idx, state) in TicketState::ALL.iter().enumerate() {
        if let Some(col) = columns.get(state) {
            for (row_idx, ticket) in col.iter().enumerate() {
                positions.insert(
                    ticket.id,
                    (
                        X_OFFSET + col_idx as f64 * COL_WIDTH,
                        Y_OFFSET + row_idx as f64 * ROW_HEIGHT,
                    ),
                );
            }
        }
    }

    // Build nodes JSON.
    let nodes_json: Vec<String> = tickets
        .iter()
        .map(|t| {
            let (x, y) = positions.get(&t.id).copied().unwrap_or((0.0, 0.0));
            let label = format!(
                "<div class=\"fnode\"><a href=\"/t/{}\" class=\"fnode-id\">{}</a>\
                 <div class=\"fnode-title\">{}</div>\
                 <span class=\"fnode-state state-{}\">{}</span></div>",
                escape(&t.short_id()),
                escape(&t.short_id()),
                escape(&flatten(&t.title)),
                escape(t.state.as_str()),
                escape(t.state.as_str()),
            );
            format!(
                "{{\"id\":\"{}\",\"data\":{{\"label\":{}}},\"position\":{{\"x\":{},\"y\":{}}},\"type\":\"default\",\"draggable\":false}}",
                escape(&t.id.to_string()),
                serde_json::to_string(&label).unwrap_or_else(|_| "\"\"".to_string()),
                x,
                y
            )
        })
        .collect();

    // Build edges JSON: depends_on (solid) + parent→child (dashed).
    let mut edges: Vec<String> = Vec::new();
    for t in tickets {
        for dep in &t.depends_on {
            if present.contains(dep) {
                edges.push(format!(
                    "{{\"id\":\"d-{}-{}\",\"source\":\"{}\",\"target\":\"{}\",\"type\":\"smoothstep\",\"animated\":true}}",
                    escape(&t.id.to_string()), escape(&dep.to_string()), escape(&t.id.to_string()), escape(&dep.to_string())
                ));
            }
        }
        if let Some(parent) = t.parent {
            if present.contains(&parent) {
                edges.push(format!(
                    "{{\"id\":\"p-{}-{}\",\"source\":\"{}\",\"target\":\"{}\",\"type\":\"smoothstep\",\"animated\":false,\"style\":{{\"strokeDasharray\":\"5 5\"}}}}",
                    escape(&parent.to_string()), escape(&t.id.to_string()), escape(&parent.to_string()), escape(&t.id.to_string())
                ));
            }
        }
    }

    let nodes_json = nodes_json.join(",");
    let edges_json = edges.join(",");

    body.push_str(&format!(
        "<div class=\"flow-wrap\" id=\"flow\"></div>\
         <p class=\"count\">{} ticket{} \u{b7} {} edge{} \u{b7} read-only \u{b7} drag to pan, scroll to zoom</p>",
        tickets.len(),
        if tickets.len() == 1 { "" } else { "s" },
        edges.len(),
        if edges.len() == 1 { "" } else { "s" },
    ));

    flow_document(
        &format!("{} flow", page.repo),
        &body,
        &nodes_json,
        &edges_json,
    )
}

/// Like `document` but injects React Flow CSS and an ESM import map into
/// the `<head>`, plus the bootstrap script at the end of `<body>`.
fn flow_document(title: &str, body: &str, nodes: &str, edges: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title>\
         <style>{STYLE}</style>\
         <link rel=\"stylesheet\" href=\"https://esm.sh/@xyflow/react@12/dist/style.css\">\
         <script type=\"importmap\">{{\
           \"imports\":{{\
             \"react\":\"https://esm.sh/react@18.3.1\",\
             \"react-dom/client\":\"https://esm.sh/react-dom@18.3.1/client\",\
             \"@xyflow/react\":\"https://esm.sh/@xyflow/react@12?deps=react@18.3.1,react-dom@18.3.1\"\
           }}\
         }}</script></head>\
         <body><main>{body}</main>\
         <script type=\"module\">{SCRIPT}</script></body></html>\n",
        title = escape(title),
        STYLE = super::STYLE,
        SCRIPT = bootstrap_script(nodes, edges),
    )
}

fn bootstrap_script(nodes: &str, edges: &str) -> String {
    format!(
        r#"import React from 'react';
import {{ createRoot }} from 'react-dom/client';
import {{ ReactFlow, Background, Controls, MarkerType }} from '@xyflow/react';

const nodes = [{nodes}];
const edges = [{edges}].map(e => ({{...e, markerEnd: {{type: MarkerType.ArrowClosed}}}}));

function Flow() {{
  return React.createElement(ReactFlow, {{
    nodes,
    edges,
    fitView: true,
    nodesDraggable: false,
    nodesConnectable: false,
    elementsSelectable: false,
    panOnDrag: true,
    zoomOnScroll: true,
    minZoom: 0.2,
    maxZoom: 2,
  }},
    React.createElement(Background, {{ variant: 'dots', gap: 16, size: 1 }}),
    React.createElement(Controls, {{ showInteractive: false }}),
  );
}}

const root = createRoot(document.getElementById('flow'));
root.render(React.createElement(Flow));
"#
    )
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::NickMap;
    use std::collections::{BTreeMap, BTreeSet};
    use ticgit_lib::TicketState;
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
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn flow_page_contains_react_flow_imports() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::New,
        );
        let html = flow_page(&page(), &ListQuery::default(), &[t]);
        assert!(html.contains("esm.sh/@xyflow/react"));
        assert!(html.contains("importmap"));
        assert!(html.contains("ReactFlow"));
    }

    #[test]
    fn flow_page_includes_node_data() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::New,
        );
        let html = flow_page(&page(), &ListQuery::default(), &[t]);
        assert!(html.contains("d7f2d8f6-d6ec-3da1-a180-0a33fb090d59"));
        assert!(html.contains("fix parser"));
    }

    #[test]
    fn flow_page_shows_empty_message_when_no_tickets() {
        let html = flow_page(&page(), &ListQuery::default(), &[]);
        assert!(html.contains("No tickets match"));
    }

    #[test]
    fn flow_page_renders_dependency_edges() {
        let mut a = ticket(
            "aaaaaaaa-d6ec-3da1-a180-0a33fb090d59",
            "parent task",
            TicketState::New,
        );
        let mut b = ticket(
            "bbbbbbbb-d6ec-3da1-a180-0a33fb090d59",
            "child task",
            TicketState::InProgress,
        );
        b.depends_on.insert(a.id);
        let html = flow_page(&page(), &ListQuery::default(), &[a, b]);
        assert!(html.contains("d-"));
        assert!(html.contains("smoothstep"));
    }
}
