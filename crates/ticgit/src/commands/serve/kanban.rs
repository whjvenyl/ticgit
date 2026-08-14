//! Kanban board view for `ti serve`.
//!
//! Tickets are grouped into columns by lifecycle state. Within each
//! column, subissues appear nested below their parent card. Parent
//! cards are expandable — click the toggle to show/hide children.
//! Read-only: cards link to the detail page at `/t/<id>`.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use ticgit_lib::{Ticket, TicketState, TicketStatus};
use uuid::Uuid;

use super::{document, escape, flatten, tag_hue, Page, Request, Response};
use super::tickets::{header, ListQuery, View};
use crate::commands::open_store;
use crate::render;

pub(super) fn response(request: &Request) -> Result<Response> {
    let store = open_store()?;
    let query = ListQuery::from_request(request);
    // Kanban is a board — always show subissues so the full work
    // picture is visible, regardless of the default hide behavior.
    let mut filter = query.filter()?;
    filter.hide_subissues = false;
    let tickets = ticgit_lib::query::apply(store.list()?, &filter);
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

    // Set of ticket IDs present in this view (for detecting orphan subissues).
    let present: HashSet<Uuid> = tickets.iter().map(|t| t.id).collect();

    if tickets.is_empty() {
        body.push_str("<p class=\"empty\">No tickets match this view.</p>");
    } else {
        body.push_str("<div class=\"kanban\">");
        for state in TicketState::ALL {
            let col = columns.get(state);
            let count = col.map(|c| c.len()).unwrap_or(0);
            if count == 0 && state.status() == TicketStatus::Closed {
                continue;
            }
            body.push_str(&format!(
                "<div class=\"kanban-col\"><h3>{} <span class=\"n\">{}</span></h3><div class=\"kanban-cards\">",
                escape(state.as_str()),
                count
            ));
            if let Some(col) = col {
                // Build a map of parent_id -> children in this column.
                let mut children_of: HashMap<Uuid, Vec<&&Ticket>> = HashMap::new();
                for t in col {
                    if let Some(pid) = t.parent {
                        children_of.entry(pid).or_default().push(t);
                    }
                }

                // Render tickets that are top-level (no parent, or parent
                // not in this view). Nested children are rendered inside
                // their parent's group recursively.
                for ticket in col {
                    let is_top_level = ticket.parent.is_none()
                        || !present.contains(&ticket.parent.unwrap());
                    if is_top_level {
                        body.push_str(&card_tree(page, ticket, &children_of, &present));
                    }
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

/// Recursively render a ticket and its children (if any). When the
/// ticket has children in this column, the card gets a toggle button
/// and the children are wrapped in a collapsible container. Children
/// that themselves have children are rendered as nested groups.
fn card_tree(
    page: &Page,
    ticket: &Ticket,
    children_of: &HashMap<Uuid, Vec<&&Ticket>>,
    present: &HashSet<Uuid>,
) -> String {
    let children = children_of.get(&ticket.id);
    let has_children = children.map(|c| !c.is_empty()).unwrap_or(false);

    if !has_children {
        return card(page, ticket);
    }

    let children = children.unwrap();
    let mut out = String::new();

    // Parent card with toggle
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

    let is_sub = ticket.parent.is_some() && present.contains(&ticket.parent.unwrap());
    let card_class = if is_sub { "kcard kcard-sub kcard-parent" } else { "kcard kcard-parent" };

    let toggle = format!(
        "<button class=\"ktoggle\" onclick=\"kanbanToggle(this)\" \
         aria-expanded=\"false\">{}<span class=\"kcount\">{}</span></button>",
        expand_icon(),
        children.len()
    );

    out.push_str(&format!(
        "<div class=\"kgroup\">\
         <a class=\"{}\" href=\"/t/{}\"><div class=\"kt\">{}</div>\
         <div class=\"km\"><span class=\"kid\">{}</span>{}{}{}{}</div></a>\
         {}<div class=\"kchildren\" style=\"display:none\">",
        card_class,
        escape(&ticket.short_id()),
        escape(&flatten(&ticket.title)),
        escape(&ticket.short_id()),
        priority,
        format!("<span class=\"kc\">[+{}]</span>", children.len()),
        assigned_html,
        tags,
        toggle,
    ));

    for child in children {
        out.push_str(&card_tree(page, child, children_of, present));
    }

    out.push_str("</div></div>");
    out
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

    let parent_html = if let Some(parent) = ticket.parent {
        format!("<span class=\"kpar\">\u{21b3} {}</span>", escape(&short_uuid(&parent)))
    } else {
        String::new()
    };

    format!(
        "<a class=\"kcard{}\" href=\"/t/{}\"><div class=\"kt\">{}</div>\
         <div class=\"km\"><span class=\"kid\">{}</span>{}{}{}{}</div></a>",
        if ticket.parent.is_some() { " kcard-sub" } else { "" },
        escape(&ticket.short_id()),
        escape(&flatten(&ticket.title)),
        escape(&ticket.short_id()),
        priority,
        parent_html,
        assigned_html,
        tags,
    )
}

fn expand_icon() -> &'static str {
    "\u{25B6}" // ▶
}

fn short_uuid(id: &Uuid) -> String {
    id.to_string().chars().take(6).collect()
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
        let html = kanban_page(&page(), &ListQuery::default(), &[open, blocked]);
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
        assert!(html.contains(">new <span"));
        assert!(!html.contains(">resolved <span"));
    }

    #[test]
    fn kanban_shows_empty_message_when_no_tickets() {
        let html = kanban_page(&page(), &ListQuery::default(), &[]);
        assert!(html.contains("No tickets match"));
    }

    #[test]
    fn kanban_nests_children_under_parent_with_toggle() {
        let parent_id = Uuid::parse_str("89b9d446-0000-0000-0000-000000000001").unwrap();
        let child_id = Uuid::parse_str("dfb09193-0000-0000-0000-000000000002").unwrap();
        let mut parent_t = ticket(
            "89b9d446-0000-0000-0000-000000000001",
            "parent issue",
            TicketState::New,
        );
        parent_t.children.insert(child_id);
        let mut child_t = ticket(
            "dfb09193-0000-0000-0000-000000000002",
            "subissue",
            TicketState::New,
        );
        child_t.parent = Some(parent_id);

        let html = kanban_page(&page(), &ListQuery::default(), &[parent_t, child_t]);

        // Parent card has toggle and kgroup wrapper
        assert!(html.contains("kgroup"));
        assert!(html.contains("kcard-parent"));
        assert!(html.contains("ktoggle"));
        assert!(html.contains("kanbanToggle"));
        // Children are in a hidden container
        assert!(html.contains("kchildren"));
        assert!(html.contains("display:none"));
        // Child card is nested inside the group
        assert!(html.contains("kcard-sub"));
        assert!(html.contains("subissue"));
    }

    #[test]
    fn kanban_shows_orphan_subissue_as_standalone() {
        // Subissue whose parent is NOT in the view
        let parent_id = Uuid::parse_str("99999999-0000-0000-0000-000000000099").unwrap();
        let mut child_t = ticket(
            "dfb09193-0000-0000-0000-000000000002",
            "orphan subissue",
            TicketState::New,
        );
        child_t.parent = Some(parent_id);

        let html = kanban_page(&page(), &ListQuery::default(), &[child_t]);

        // No kgroup div (parent not present), but has parent indicator.
        // The string "kgroup" appears in the JS, so check for the div.
        assert!(!html.contains("class=\"kgroup\""));
        assert!(html.contains("kcard-sub"));
        assert!(html.contains("kpar"));
        assert!(html.contains("999999"));
    }
}
