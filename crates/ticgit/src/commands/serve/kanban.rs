//! Kanban board view for `ti serve`.
//!
//! Tickets are grouped into columns by lifecycle state. Read-only —
//! cards link to the detail page at `/t/<id>`.

use std::collections::HashMap;

use anyhow::Result;
use ticgit_lib::{Ticket, TicketState, TicketStatus};

use super::{
    document, escape, flatten, tag_hue, Page, Request, Response,
};
use super::tickets::{header, ListQuery, View};
use crate::commands::open_store;
use crate::render;

pub(super) fn response(request: &Request) -> Result<Response> {
    let store = open_store()?;
    let query = ListQuery::from_request(request);
    let tickets = ticgit_lib::query::apply(store.list()?, &query.filter()?);
    let page = Page::new(&store)?;
    Ok(Response::html(200, kanban_page(&page, &query, &tickets)))
}

fn kanban_page(page: &Page, query: &ListQuery, tickets: &[Ticket]) -> String {
    let mut body = String::new();
    body.push_str(&header(page, query, View::Kanban));

    // Group tickets by state, preserving the canonical state order.
    let mut columns: HashMap<TicketState, Vec<&Ticket>> = HashMap::new();
    for t in tickets {
        columns.entry(t.state).or_default().push(t);
    }

    if tickets.is_empty() {
        body.push_str("<p class=\"empty\">No tickets match this view.</p>");
    } else {
        body.push_str("<div class=\"kanban\">");
        for state in TicketState::ALL {
            let col = columns.get(state);
            let count = col.map(|c| c.len()).unwrap_or(0);
            // Skip empty closed columns to reduce clutter, but always
            // show open columns so the workflow is visible.
            if count == 0 && state.status() == TicketStatus::Closed {
                continue;
            }
            body.push_str(&format!(
                "<div class=\"kanban-col\"><h3>{} <span class=\"n\">{}</span></h3><div class=\"kanban-cards\">",
                escape(state.as_str()),
                count
            ));
            if let Some(col) = col {
                for ticket in col {
                    body.push_str(&card(page, ticket));
                }
            }
            body.push_str("</div></div>");
        }
        body.push_str("</div>");
    }

    body.push_str(&format!(
        "<p class=\"count\">{} ticket{} \u{b7} read-only</p>",
        tickets.len(),
        if tickets.len() == 1 { "" } else { "s" },
    ));
    document(&format!("{} kanban", page.repo), &body)
}

fn card(page: &Page, ticket: &Ticket) -> String {
    let assigned = ticket
        .assigned
        .as_deref()
        .map(|email| render::display_name(email, Some(&page.nicks)))
        .unwrap_or_default();
    let priority = ticket
        .priority
        .map(|p| format!("<span class=\"kp\">p{p}</span>"))
        .unwrap_or_default();
    let assigned_html = if assigned.is_empty() {
        String::new()
    } else {
        format!("<span class=\"ka\">{}</span>", escape(&assigned))
    };
    let tags: String = ticket
        .tags
        .iter()
        .map(|tag| {
            format!(
                "<span class=\"tag tag-{}\">{}</span>",
                tag_hue(tag),
                escape(tag)
            )
        })
        .collect::<Vec<_>>()
        .join("");

    format!(
        "<a class=\"kcard\" href=\"/t/{}\"><div class=\"kt\">{}</div>\
         <div class=\"km\"><span class=\"kid\">{}</span>{}{}{}</div></a>",
        escape(&ticket.short_id()),
        escape(&flatten(&ticket.title)),
        escape(&ticket.short_id()),
        priority,
        assigned_html,
        tags,
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

    fn request(target: &str) -> Request {
        super::super::parse_request_line(&format!("GET {target} HTTP/1.1\r\n")).unwrap()
    }

    #[test]
    fn kanban_renders_columns_and_cards() {
        let open = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "fix parser",
            TicketState::InProgress,
        );
        let blocked = ticket(
            "aaaaaaaa-d6ec-3da1-a180-0a33fb090d59",
            "waiting on api",
            TicketState::Blocked,
        );
        let html = kanban_page(
            &page(),
            &ListQuery::default(),
            &[open, blocked],
        );
        assert!(html.contains("kanban-col"));
        assert!(html.contains("in-progress"));
        assert!(html.contains("blocked"));
        assert!(html.contains("fix parser"));
        assert!(html.contains("waiting on api"));
        assert!(html.contains("href=\"/t/d7f2d8\""));
    }

    #[test]
    fn kanban_skips_empty_closed_columns() {
        let t = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "open task",
            TicketState::New,
        );
        let html = kanban_page(&page(), &ListQuery::default(), &[t]);
        // The "new" column header should be present.
        assert!(html.contains(">new <span"));
        // No "resolved" column header (the word appears in CSS, so check
        // for the heading markup specifically).
        assert!(!html.contains(">resolved <span"));
    }

    #[test]
    fn kanban_shows_empty_message_when_no_tickets() {
        let html = kanban_page(&page(), &ListQuery::default(), &[]);
        assert!(html.contains("No tickets match"));
    }
}
