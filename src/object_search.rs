//! Search only the metadata already loaded into the object tree.
use super::*;

pub(super) struct Branch {
    pub name: String,
    pub line: gtk::Box,
    pub children: gtk::Box,
    pub disclosure: gtk::Label,
    pub icon: gtk::Image,
    pub open_icon: &'static str,
    pub closed_icon: &'static str,
    /// User expansion is independent of the temporary search expansion.
    pub expanded: Rc<Cell<bool>>,
}

pub(super) fn apply(ui: &Ui) {
    let query = ui.table_filter.text().trim().to_lowercase();
    let branches = ui.object_search_branches.borrow();
    let tables = ui.table_list_items.borrow();
    // SearchEntry can deliver a debounced signal after the tree was cleared.
    if branches.is_empty() && tables.is_empty() {
        return;
    }
    fn visit(
        parent: &gtk::Box,
        inherited: bool,
        query: &str,
        branches: &[Branch],
        tables: &[(gtk::Button, DbObject)],
    ) -> bool {
        let mut matched = false;
        for (button, object) in tables {
            if button.parent().as_ref() != Some(parent.upcast_ref()) {
                continue;
            }
            let visible = inherited || object.name.to_lowercase().contains(query);
            button.set_visible(visible);
            matched |= visible;
        }
        for branch in branches {
            if branch.line.parent().as_ref() != Some(parent.upcast_ref()) {
                continue;
            }
            let own_match = inherited || branch.name.to_lowercase().contains(query);
            let descendant_match = visit(&branch.children, own_match, query, branches, tables);
            let visible = own_match || descendant_match;
            let expanded = if query.is_empty() {
                branch.expanded.get()
            } else {
                visible
            };
            branch.line.set_visible(visible);
            branch.children.set_visible(visible && expanded);
            branch.disclosure.set_text(if expanded { "▾" } else { "▸" });
            set_penpot_icon(
                &branch.icon,
                if expanded {
                    branch.open_icon
                } else {
                    branch.closed_icon
                },
                false,
            );
            matched |= visible;
        }
        matched
    }
    let matched = visit(&ui.objects, query.is_empty(), &query, &branches, &tables);
    let mut child = ui.objects.first_child();
    let mut empty = None;
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.widget_name() == "object-search-empty" {
            empty = Some(widget);
            break;
        }
    }
    let empty = empty.unwrap_or_else(|| {
        let label = gtk::Label::new(Some("Нет совпадений в загруженном каталоге"));
        label.set_widget_name("object-search-empty");
        label.set_wrap(true);
        label.add_css_class("dim-label");
        ui.objects.append(&label);
        label.upcast()
    });
    empty.set_visible(!query.is_empty() && !matched);
}
