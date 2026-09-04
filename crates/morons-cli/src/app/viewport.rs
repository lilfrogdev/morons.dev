use morons_protocol::{MessageId, RunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptBlockKey {
    Entry(MessageId),
    Transient(RunId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TranscriptBlockMetric {
    key: TranscriptBlockKey,
    start: usize,
    height: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TranscriptAnchor {
    key: TranscriptBlockKey,
    row: usize,
}

#[derive(Debug)]
pub(super) struct TranscriptViewport {
    follow_latest: bool,
    top: usize,
    width: u16,
    height: u16,
    content_height: usize,
    blocks: Vec<TranscriptBlockMetric>,
    layout_dirty: bool,
    newer_output: bool,
}

impl Default for TranscriptViewport {
    fn default() -> Self {
        Self {
            follow_latest: true,
            top: 0,
            width: 0,
            height: 0,
            content_height: 0,
            blocks: Vec::new(),
            layout_dirty: true,
            newer_output: false,
        }
    }
}

impl TranscriptViewport {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) const fn needs_measurement(&self, width: u16) -> bool {
        self.layout_dirty || self.width != width
    }

    pub(super) fn update_layout(
        &mut self,
        width: u16,
        height: u16,
        measured: Option<Vec<(TranscriptBlockKey, usize)>>,
    ) {
        let anchor = if self.follow_latest {
            None
        } else {
            self.anchor()
        };
        if let Some(measured) = measured {
            let mut start = 0_usize;
            self.blocks = measured
                .into_iter()
                .map(|(key, height)| {
                    let height = height.max(1);
                    let metric = TranscriptBlockMetric { key, start, height };
                    start = start.saturating_add(height);
                    metric
                })
                .collect();
            self.content_height = start;
            self.width = width;
            self.layout_dirty = false;
        }
        self.height = height;
        let maximum_top = self.maximum_top();
        if self.follow_latest {
            self.top = maximum_top;
            self.newer_output = false;
            return;
        }
        if maximum_top == 0 {
            self.follow_latest = true;
            self.top = 0;
            self.newer_output = false;
            return;
        }
        self.top = anchor
            .and_then(|anchor| {
                self.blocks
                    .iter()
                    .find(|metric| metric.key == anchor.key)
                    .map(|metric| {
                        metric
                            .start
                            .saturating_add(anchor.row.min(metric.height.saturating_sub(1)))
                    })
            })
            .unwrap_or(self.top)
            .min(maximum_top);
    }

    pub(super) fn note_content_changed(&mut self) {
        self.note_layout_changed();
        if !self.follow_latest {
            self.newer_output = true;
        }
    }

    pub(super) fn note_layout_changed(&mut self) {
        self.layout_dirty = true;
    }

    pub(super) fn scroll_lines_up(&mut self, rows: usize) {
        let current = if self.follow_latest {
            self.maximum_top()
        } else {
            self.top
        };
        let next = current.saturating_sub(rows);
        if next < current {
            self.follow_latest = false;
            self.top = next;
        }
    }

    pub(super) fn scroll_lines_down(&mut self, rows: usize) {
        if self.follow_latest {
            return;
        }
        let maximum_top = self.maximum_top();
        self.top = self.top.saturating_add(rows).min(maximum_top);
        if self.top == maximum_top {
            self.scroll_to_bottom();
        }
    }

    pub(super) fn scroll_page_up(&mut self) {
        self.scroll_lines_up(self.page_rows());
    }

    pub(super) fn scroll_page_down(&mut self) {
        self.scroll_lines_down(self.page_rows());
    }

    pub(super) fn scroll_to_top(&mut self) {
        if self.maximum_top() > 0 {
            self.follow_latest = false;
            self.top = 0;
        }
    }

    pub(super) fn scroll_to_bottom(&mut self) {
        self.follow_latest = true;
        self.top = self.maximum_top();
        self.newer_output = false;
    }

    pub(super) const fn follows_latest(&self) -> bool {
        self.follow_latest
    }

    pub(super) const fn has_newer_output(&self) -> bool {
        self.newer_output
    }

    pub(super) const fn top(&self) -> usize {
        self.top
    }

    pub(super) const fn content_height(&self) -> usize {
        self.content_height
    }

    pub(super) const fn viewport_height(&self) -> usize {
        self.height as usize
    }

    fn maximum_top(&self) -> usize {
        self.content_height.saturating_sub(usize::from(self.height))
    }

    fn page_rows(&self) -> usize {
        usize::from(self.height).saturating_sub(1).max(1)
    }

    fn anchor(&self) -> Option<TranscriptAnchor> {
        self.blocks
            .iter()
            .find(|metric| {
                self.top >= metric.start && self.top < metric.start.saturating_add(metric.height)
            })
            .map(|metric| TranscriptAnchor {
                key: metric.key,
                row: self.top.saturating_sub(metric.start),
            })
    }
}
