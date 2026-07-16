use std::collections::HashSet;

use perseval_service::SpanRow;

use crate::controllers::BoundedPageCache;

pub(super) const TIMELINE_PAGE_SIZE: u64 = 500;

#[derive(Debug, Clone)]
pub(super) struct FullTraceTimelineModel {
    pages: BoundedPageCache<u64, Vec<SpanRow>>,
    loading: HashSet<u64>,
    total: u64,
}

impl FullTraceTimelineModel {
    pub fn new(cached_pages: usize) -> Self {
        Self {
            pages: BoundedPageCache::new(cached_pages),
            loading: HashSet::new(),
            total: 0,
        }
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.loading.clear();
        self.total = 0;
    }

    pub fn set_total(&mut self, total: u64) {
        self.total = total;
    }

    pub fn total(&self) -> usize {
        usize::try_from(self.total).unwrap_or(usize::MAX)
    }

    pub fn begin_load(&mut self, page: u64) -> bool {
        !self.pages.contains_key(&page) && self.loading.insert(page)
    }

    pub fn finish_load(&mut self, page: u64, rows: Vec<SpanRow>) {
        self.loading.remove(&page);
        self.pages.insert(page, rows);
    }

    pub fn fail_load(&mut self, page: u64) {
        self.loading.remove(&page);
    }

    pub fn row(&self, index: usize) -> Option<&SpanRow> {
        let page = index as u64 / TIMELINE_PAGE_SIZE;
        let index = index % TIMELINE_PAGE_SIZE as usize;
        self.pages.peek(&page)?.get(index)
    }

    pub fn loaded_count(&self) -> usize {
        self.pages.values().map(Vec::len).sum()
    }

    pub fn loaded_rows(&self) -> Vec<SpanRow> {
        let mut rows = self
            .pages
            .values()
            .flat_map(|page| page.iter().cloned())
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.start_time_unix_nano
                .cmp(&right.start_time_unix_nano)
                .then_with(|| left.span_id.cmp(&right.span_id))
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn row(id: &str, start: u64) -> SpanRow {
        SpanRow {
            logical_trace_id: "trace".into(),
            revision: 1,
            span_id: id.into(),
            parent_span_id: None,
            name: id.into(),
            category: "agent".into(),
            start_time_unix_nano: start,
            duration_nano: 1,
            status_code: 0,
            status_message: String::new(),
            depth: 0,
            has_children: false,
            attributes: BTreeMap::new(),
            payload_refs: BTreeMap::new(),
            events: Vec::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn timeline_rows_do_not_depend_on_tree_expansion() {
        let mut timeline = FullTraceTimelineModel::new(8);
        timeline.set_total(2);
        timeline.finish_load(0, vec![row("parent", 10), row("child", 20)]);

        assert_eq!(timeline.total(), 2);
        assert_eq!(
            timeline.row(1).map(|row| row.span_id.as_str()),
            Some("child")
        );
    }

    #[test]
    fn retained_pages_are_bounded() {
        let mut timeline = FullTraceTimelineModel::new(2);
        timeline.finish_load(0, vec![row("zero", 0)]);
        timeline.finish_load(1, vec![row("one", 1)]);
        timeline.finish_load(2, vec![row("two", 2)]);

        assert!(timeline.row(0).is_none());
        assert_eq!(timeline.loaded_count(), 2);
    }
}
