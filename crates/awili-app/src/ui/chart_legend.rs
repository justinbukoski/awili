//! `ui::ChartLegend` — the kit's chart legend (KIT.md v3, batch K4).
//!
//! Legend swatches — `DOT_L` **squares** in the `chart_palette`
//! content colours — with `type_::LABEL` labels. The swatch colours are
//! chart data (the same content module the usage chart paints from), so
//! the kit keeps reading its UI chrome from the palette while the
//! series colours come from [`crate::chart_palette`].

use gpui::{div, prelude::*, App, ElementId, IntoElement, RenderOnce, Rgba, SharedString, Window};

use crate::chart_palette;
use crate::theme;
use crate::ui::interaction::Common;
use crate::ui::text;

/// The part tag for the default `debug_selector`.
const CHART_LEGEND: &str = "chart-legend";
/// The swatch's corner rounding, in logical px before zoom (a soft
/// square, not a pill).
const SWATCH_RADIUS: f32 = theme::CHART_SWATCH_RADIUS;

/// One legend entry: a swatch colour and its label.
#[derive(Clone, Debug)]
pub struct ChartLegendItem {
    label: SharedString,
    color: Rgba,
}

impl ChartLegendItem {
    /// A legend entry with an explicit swatch colour (the parent's
    /// choice — usually a `chart_palette` series colour).
    pub fn new(label: impl Into<SharedString>, color: Rgba) -> Self {
        Self {
            label: label.into(),
            color,
        }
    }

    /// The input-token series (the `chart_palette::INPUT` blue).
    pub fn input(label: impl Into<SharedString>) -> Self {
        Self::new(label, chart_palette::INPUT)
    }

    /// The output-token series (the `chart_palette::OUTPUT` sage).
    pub fn output(label: impl Into<SharedString>) -> Self {
        Self::new(label, chart_palette::OUTPUT)
    }

    /// The entry's label.
    pub fn label(&self) -> &SharedString {
        &self.label
    }

    /// The entry's swatch colour.
    pub fn color(&self) -> Rgba {
        self.color
    }
}

/// A kit chart legend. Build with [`ChartLegend::new`] and the entries;
/// the part is non-interactive (the chart it labels owns the state).
#[derive(IntoElement)]
pub struct ChartLegend {
    common: Common,
    items: Vec<ChartLegendItem>,
}

impl ChartLegend {
    /// A legend with the given entries (swatch square + label each).
    pub fn new(id: impl Into<ElementId>, items: Vec<ChartLegendItem>) -> Self {
        Self {
            common: Common {
                id: id.into(),
                ..Default::default()
            },
            items,
        }
    }

    /// The entries, in paint order.
    pub fn items(&self) -> &[ChartLegendItem] {
        &self.items
    }

    /// The `debug_selector` override (KIT.md § Conventions).
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.common.selector = Some(selector);
        self
    }

    /// The accessible label (the tooltip when none is set).
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.common.label = Some(text.into());
        self
    }
}

impl std::fmt::Debug for ChartLegend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ChartLegend({:?})",
            self.common.selector_for(CHART_LEGEND)
        )
    }
}

impl RenderOnce for ChartLegend {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let common = self.common;
        let selector = common.selector_for(CHART_LEGEND);

        let mut row = div()
            .id(common.id.clone())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme::z(theme::SP_8))
            .debug_selector(|| selector.clone());
        for (i, item) in self.items.iter().enumerate() {
            row = row.child(
                div()
                    .id(crate::ui::interaction::child_id(
                        &common.id,
                        &format!("item-{i}"),
                    ))
                    .debug_selector(|| format!("{selector}-item-{i}"))
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(theme::z(theme::SP_4))
                    .child(
                        div()
                            .size(theme::z(theme::DOT_L))
                            .rounded(theme::z(SWATCH_RADIUS))
                            .bg(item.color),
                    )
                    // The label truncates with its full text as the tooltip.
                    .child(text::label(item.label.clone()).dim().id(
                        crate::ui::interaction::child_id(&common.id, &format!("item-{i}-label")),
                    )),
            );
        }
        // The labels read `TEXT_DIM` (the kit's dim text); the
        // swatches carry the series colours.
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    use crate::ui::interaction::test_util::render_part;

    /// The legend lays out headless with one swatch per entry (a
    /// `DOT_L` square) and its `type_::LABEL` labels.
    #[gpui::test]
    fn chart_legend_renders_swatches_and_labels(cx: &mut TestAppContext) {
        let (_root, wcx) = render_part(cx, move || {
            div().flex().child(ChartLegend::new(
                "legend",
                vec![
                    ChartLegendItem::input("Input tokens"),
                    ChartLegendItem::output("Output tokens"),
                ],
            ))
        });
        let legend = wcx
            .debug_bounds("ui-chart-legend-legend")
            .unwrap_or_else(|| panic!("the legend recorded no bounds"));
        assert!(
            f32::from(legend.size.width) > 0.0 && f32::from(legend.size.height) > 0.0,
            "the legend has size: {legend:?}"
        );
        for sel in [
            "ui-chart-legend-legend-item-0",
            "ui-chart-legend-legend-item-1",
        ] {
            assert!(wcx.debug_bounds(sel).is_some(), "{sel} laid out");
        }
    }

    /// The series helpers pick the chart_palette content colours
    /// (input blue, output sage) verbatim.
    #[test]
    fn chart_legend_series_use_the_chart_palette() {
        assert_eq!(
            ChartLegendItem::input("in").color(),
            chart_palette::INPUT,
            "the input entry carries the chart palette's input colour"
        );
        assert_eq!(
            ChartLegendItem::output("out").color(),
            chart_palette::OUTPUT,
            "the output entry carries the chart palette's output colour"
        );
    }
}
