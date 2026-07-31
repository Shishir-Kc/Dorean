//! A filterable list overlay used by the model/stack/session selectors.

/// A filterable select list.
#[derive(Debug, Clone)]
pub struct SelectList {
    pub title: String,
    pub items: Vec<String>,
    pub selected: usize,
    /// Simple substring filter over item text.
    pub filter: String,
    /// How many rows the overlay can display.
    pub viewport: usize,
    /// Scroll offset into the filtered items.
    pub scroll: usize,
}

impl SelectList {
    pub fn new(title: impl Into<String>, items: Vec<String>) -> Self {
        SelectList {
            title: title.into(),
            items,
            selected: 0,
            filter: String::new(),
            viewport: 10,
            scroll: 0,
        }
    }

    /// Items matching the current filter (case-insensitive substring).
    pub fn filtered(&self) -> Vec<&str> {
        if self.filter.is_empty() {
            return self.items.iter().map(String::as_str).collect();
        }
        let needle = self.filter.to_ascii_lowercase();
        self.items
            .iter()
            .filter(|item| item.to_ascii_lowercase().contains(&needle))
            .map(String::as_str)
            .collect()
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn clear_filter(&mut self) {
        self.set_filter(String::new());
    }

    pub fn select_next(&mut self) {
        let count = self.filtered().len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected + 1) % count;
        self.keep_visible();
    }

    pub fn select_prev(&mut self) {
        let count = self.filtered().len();
        if count == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            count - 1
        } else {
            self.selected - 1
        };
        self.keep_visible();
    }

    pub fn page_next(&mut self) {
        for _ in 0..self.viewport {
            self.select_next();
        }
    }

    pub fn page_prev(&mut self) {
        for _ in 0..self.viewport {
            self.select_prev();
        }
    }

    /// The currently selected filtered item, if any.
    pub fn selected_value(&self) -> Option<&str> {
        self.filtered().get(self.selected).copied()
    }

    fn keep_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.viewport {
            self.scroll = self.selected + 1 - self.viewport;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_case_insensitively() {
        let list = SelectList::new(
            "models",
            vec!["gpt-4o".into(), "llama-3.3".into(), "deepseek-r1".into()],
        );
        let filtered = list.filtered();
        assert_eq!(filtered.len(), 3);
        let mut list = list;
        list.set_filter("LLAMA".into());
        assert_eq!(list.filtered(), vec!["llama-3.3"]);
    }

    #[test]
    fn selection_wraps() {
        let mut list = SelectList::new("x", vec!["a".into(), "b".into(), "c".into()]);
        list.select_prev();
        assert_eq!(list.selected_value(), Some("c"));
        list.select_next();
        assert_eq!(list.selected_value(), Some("a"));
        list.select_next();
        assert_eq!(list.selected_value(), Some("b"));
    }

    #[test]
    fn filtering_resets_selection() {
        let mut list = SelectList::new("x", vec!["ab".into(), "ac".into(), "bd".into()]);
        list.select_next();
        list.select_next();
        list.set_filter("a".into());
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn viewport_scroll_follows_selection() {
        let mut list = SelectList::new("x", (0..50).map(|i| format!("item {i}")).collect());
        list.viewport = 5;
        for _ in 0..10 {
            list.select_next();
        }
        assert_eq!(list.scroll, 6);
        for _ in 0..10 {
            list.select_prev();
        }
        assert_eq!(list.scroll, 0);
    }
}
