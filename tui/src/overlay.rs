//! Geometry helpers for the centered popups that the status, help and
//! search views are drawn in. Pure functions so the layout rules can be
//! tested without a terminal.

use ratatui::layout::Rect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns and rows of the base layer kept visible on each side of a popup,
/// so the task tree underneath stays recognisable as a background layer.
const MIN_SIDE_MARGIN: u16 = 2;
const MIN_VERTICAL_MARGIN: u16 = 1;

/// Marks a truncated title. One display column wide.
const ELLIPSIS: char = '…';

/// Centers a popup of `width_pct` / `height_pct` percent of `area` inside
/// it, shrinking it when needed to keep the base layer visible around it.
pub fn centered_rect(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let width = popup_len(area.width, width_pct, MIN_SIDE_MARGIN);
    let height = popup_len(area.height, height_pct, MIN_VERTICAL_MARGIN);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn popup_len(available: u16, pct: u16, min_margin: u16) -> u16 {
    let wanted = (u32::from(available) * u32::from(pct) / 100) as u16;
    // A terminal too small for the margins still gets a popup: showing the
    // modal at all matters more than letting the background peek through.
    let capped = available.saturating_sub(min_margin * 2).max(1);
    wanted.clamp(1, capped).min(available)
}

/// Cuts `text` to `max_width` display columns, marking the cut with an
/// ellipsis. Used for popup titles, which the border clips silently
/// otherwise.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = max_width - ELLIPSIS.width().unwrap_or(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > budget {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push(ELLIPSIS);
    out
}

/// First visible index that keeps `selected` inside a viewport of `height`
/// rows, scrolling as late as possible. Recomputed per frame, so views that
/// store only a cursor still follow it.
pub fn scroll_to_show(selected: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    selected.saturating_sub(height - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests the basic centering and sizing of a popup.
    // Given: a 100x40 area and a request for 80% of both dimensions
    // When: the popup rect is computed
    // Then: it is 80x32 and sits centered, leaving equal margins
    #[test]
    fn centered_rect_applies_percentages_and_centers() {
        let popup = centered_rect(Rect::new(0, 0, 100, 40), 80, 80);

        assert_eq!(popup, Rect::new(10, 4, 80, 32));
    }

    // Tests centering when the leftover space cannot be split evenly.
    // Given: a 101x41 area and a request for 80% of both dimensions
    // When: the popup rect is computed
    // Then: the odd column/row goes to the right and bottom side, and the
    //       popup stays fully inside the area
    #[test]
    fn centered_rect_puts_odd_leftover_on_the_far_side() {
        let area = Rect::new(0, 0, 101, 41);

        let popup = centered_rect(area, 80, 80);

        assert_eq!(popup, Rect::new(10, 4, 80, 32));
        assert_eq!(popup.right() + 11, area.right());
        assert_eq!(popup.bottom() + 5, area.bottom());
    }

    // Tests that the popup never covers the whole area.
    // Given: a small 10x4 area and a request for 100% of both dimensions
    // When: the popup rect is computed
    // Then: at least two columns and one row of the base layer survive on
    //       every side
    #[test]
    fn centered_rect_clamps_to_keep_the_base_layer_visible() {
        let area = Rect::new(0, 0, 10, 4);

        let popup = centered_rect(area, 100, 100);

        assert_eq!(popup, Rect::new(2, 1, 6, 2));
        assert_eq!(popup.x - area.x, 2);
        assert_eq!(area.right() - popup.right(), 2);
        assert_eq!(popup.y - area.y, 1);
        assert_eq!(area.bottom() - popup.bottom(), 1);
    }

    // Tests the degenerate case of a terminal too small for the margins.
    // Given: a 3x1 area, which cannot fit the side margins at all
    // When: the popup rect is computed
    // Then: a one-cell popup is still produced, inside the area
    #[test]
    fn centered_rect_survives_a_tiny_area() {
        let area = Rect::new(0, 0, 3, 1);

        let popup = centered_rect(area, 80, 80);

        assert_eq!(popup, Rect::new(1, 0, 1, 1));
        assert!(popup.right() <= area.right() && popup.bottom() <= area.bottom());
    }

    // Tests that an empty area cannot produce an out-of-bounds popup.
    // Given: a 0x0 area
    // When: the popup rect is computed
    // Then: the popup is empty too
    #[test]
    fn centered_rect_of_empty_area_is_empty() {
        let popup = centered_rect(Rect::new(0, 0, 0, 0), 80, 80);

        assert_eq!(popup.width, 0);
        assert_eq!(popup.height, 0);
    }

    // Tests that the popup is positioned relative to the given area.
    // Given: an area offset to x=5, y=2 and a request for 50%
    // When: the popup rect is computed
    // Then: the popup is centered within that offset area, not the screen
    #[test]
    fn centered_rect_respects_the_area_origin() {
        let popup = centered_rect(Rect::new(5, 2, 20, 10), 50, 50);

        assert_eq!(popup, Rect::new(10, 4, 10, 5));
    }

    // Tests that a title that fits is left alone.
    // Given: a title of 5 columns and a 10 column budget
    // When: truncating
    // Then: the title is returned unchanged
    #[test]
    fn truncate_keeps_text_that_fits() {
        assert_eq!(truncate_to_width("search", 10), "search");
        assert_eq!(truncate_to_width("search", 6), "search");
    }

    // Tests truncation of an overlong title.
    // Given: a title longer than the budget
    // When: truncating to 6 columns
    // Then: the result ends with an ellipsis and fits the budget exactly
    #[test]
    fn truncate_marks_the_cut_and_fits_the_budget() {
        let cut = truncate_to_width("search: design | sort: due", 6);

        assert_eq!(cut, "searc…");
        assert_eq!(cut.width(), 6);
    }

    // Tests truncation in the middle of a double-width character.
    // Given: a title of CJK characters, each two columns wide
    // When: truncating to 5 columns
    // Then: the last character is dropped rather than half-drawn, so the
    //       result is 3 columns wide and still fits
    #[test]
    fn truncate_never_splits_a_double_width_char() {
        let cut = truncate_to_width("設計課題", 5);

        assert_eq!(cut, "設計…");
        assert!(cut.width() <= 5);
    }

    // Tests the no-room case.
    // Given: a zero column budget
    // When: truncating any non-empty text
    // Then: nothing is returned, since even the ellipsis would not fit
    #[test]
    fn truncate_to_zero_width_is_empty() {
        assert_eq!(truncate_to_width("search", 0), "");
    }

    // Tests that a selection already on screen does not scroll the view.
    // Given: a viewport of 10 rows and a selection within the first 10 rows
    // When: the scroll offset is computed
    // Then: the view stays at the top
    #[test]
    fn scroll_stays_at_top_while_the_selection_fits() {
        assert_eq!(scroll_to_show(0, 10), 0);
        assert_eq!(scroll_to_show(9, 10), 0);
    }

    // Tests scrolling past the bottom of the viewport.
    // Given: a viewport of 10 rows and a selection on row 12
    // When: the scroll offset is computed
    // Then: the view scrolls just far enough to show the selection last
    #[test]
    fn scroll_follows_the_selection_past_the_bottom() {
        assert_eq!(scroll_to_show(10, 10), 1);
        assert_eq!(scroll_to_show(12, 10), 3);
    }

    // Tests the degenerate viewport.
    // Given: a viewport with no rows at all
    // When: the scroll offset is computed
    // Then: it is zero, since nothing can be shown anyway
    #[test]
    fn scroll_of_zero_height_viewport_is_zero() {
        assert_eq!(scroll_to_show(5, 0), 0);
    }
}
