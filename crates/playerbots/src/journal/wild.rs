//! The `wild` section of the journal: plots a bot planted out in the open (owned by the wild-farm
//! feature). The server lists a character's crops on the public farms (`CSRequestCommonFarmList`)
//! but nothing else, so a bot that restarts finds its wild plots again only through this.
//!
//! The farm goal writes a plot here when it plants on wild land, drops it when it harvests it, and
//! drops it as stolen when the bot comes back and the crop is gone. The patch the bot uses (the
//! middle of its small field) is kept too, and so are the patches where crops were stolen, so the
//! next field goes somewhere else.

use serde::{Deserialize, Serialize};

/// Plots kept at most.
pub const MAX_WILD_PLOTS: usize = 32;
/// Patches where crops were stolen kept at most.
pub const MAX_BAD_PATCHES: usize = 8;
/// A plot older than this (seconds) is forgotten: the crop has withered and gone by then.
pub const MAX_PLOT_AGE_SECS: u64 = 7 * 86_400;
/// Two plots this close (metres) are the same plot.
pub const SAME_PLOT_M: f32 = 0.75;

/// One plot out in the wild.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WildPlot {
    /// Where it stands.
    pub pos: [f32; 3],
    /// The seed item that was planted.
    pub seed: u32,
    /// The crop's doodad template.
    pub doodad: u32,
    /// Unix seconds of the planting.
    pub planted_unix: u64,
    /// Unix seconds from which it should be ripe.
    pub ripe_unix: u64,
    /// The seed was the bot's own (bought): the harvest may be sold.
    pub honest: bool,
}

/// The bot's wild plots.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WildPlots {
    /// The plots, oldest first, at most [`MAX_WILD_PLOTS`].
    pub plots: Vec<WildPlot>,
    /// The middle of the patch the bot plants on now.
    pub patch: Option<[f32; 2]>,
    /// Patches where crops were stolen, oldest first, at most [`MAX_BAD_PATCHES`].
    pub bad_patches: Vec<[f32; 2]>,
    /// Crops harvested.
    pub harvested: u32,
    /// Crops gone when the bot came back for them.
    pub stolen: u32,
}

fn near(a: [f32; 3], b: [f32; 3], m: f32) -> bool {
    let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
    dx * dx + dy * dy < m * m
}

impl WildPlots {
    /// Records a plot (replacing one on the same spot); the oldest goes when the section is full.
    pub fn add(&mut self, plot: WildPlot) {
        self.plots.retain(|p| !near(p.pos, plot.pos, SAME_PLOT_M));
        if self.plots.len() >= MAX_WILD_PLOTS {
            self.plots.remove(0);
        }
        self.plots.push(plot);
    }

    /// Forgets the plot at `pos`; returns it.
    pub fn remove_at(&mut self, pos: [f32; 3]) -> Option<WildPlot> {
        let i = self
            .plots
            .iter()
            .position(|p| near(p.pos, pos, SAME_PLOT_M))?;
        Some(self.plots.remove(i))
    }

    /// The plot at `pos` was harvested.
    pub fn harvested_at(&mut self, pos: [f32; 3]) -> bool {
        let found = self.remove_at(pos).is_some();
        if found {
            self.harvested = self.harvested.saturating_add(1);
        }
        found
    }

    /// The crop at `pos` was gone when the bot came back: it counts as stolen, its patch goes on
    /// the list of bad patches and the bot looks for another one.
    pub fn stolen_at(&mut self, pos: [f32; 3]) -> bool {
        if self.remove_at(pos).is_none() {
            return false;
        }
        self.stolen = self.stolen.saturating_add(1);
        if let Some(patch) = self.patch.take() {
            self.bad_patches.retain(|b| *b != patch);
            if self.bad_patches.len() >= MAX_BAD_PATCHES {
                self.bad_patches.remove(0);
            }
            self.bad_patches.push(patch);
        }
        true
    }

    /// Whether (x, y) is within `radius` metres of a patch where crops were stolen.
    pub fn near_bad_patch(&self, x: f32, y: f32, radius: f32) -> bool {
        self.bad_patches
            .iter()
            .any(|b| near([b[0], b[1], 0.0], [x, y, 0.0], radius))
    }

    /// Forgets the plots planted more than [`MAX_PLOT_AGE_SECS`] before `now_unix`; returns how
    /// many went.
    pub fn prune(&mut self, now_unix: u64) -> usize {
        let before = self.plots.len();
        self.plots
            .retain(|p| p.planted_unix + MAX_PLOT_AGE_SECS > now_unix);
        if self.plots.is_empty() && before > 0 {
            self.patch = None;
        }
        before - self.plots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plot(x: f32, planted: u64) -> WildPlot {
        WildPlot {
            pos: [x, 0.0, 100.0],
            seed: 1,
            doodad: 2,
            planted_unix: planted,
            ripe_unix: planted + 600,
            honest: true,
        }
    }

    #[test]
    fn plots_are_kept_once_per_spot_and_bounded() {
        let mut w = WildPlots::default();
        w.add(plot(0.0, 10));
        w.add(plot(0.3, 20));
        assert_eq!(w.plots.len(), 1, "the same spot");
        assert_eq!(w.plots[0].planted_unix, 20);
        for i in 0..(MAX_WILD_PLOTS + 3) {
            w.add(plot(10.0 + i as f32 * 2.0, 30));
        }
        assert_eq!(w.plots.len(), MAX_WILD_PLOTS);
        assert!(
            w.remove_at([0.0, 0.0, 0.0]).is_none(),
            "the oldest went first"
        );
    }

    #[test]
    fn a_harvest_and_a_theft_are_counted_and_a_robbed_patch_is_remembered() {
        let mut w = WildPlots {
            patch: Some([5.0, 5.0]),
            ..WildPlots::default()
        };
        w.add(plot(0.0, 10));
        w.add(plot(4.0, 10));
        assert!(w.harvested_at([0.1, 0.0, 0.0]));
        assert!(!w.harvested_at([0.1, 0.0, 0.0]), "already gone");
        assert_eq!(w.harvested, 1);
        assert!(w.stolen_at([4.0, 0.2, 0.0]));
        assert_eq!(w.stolen, 1);
        assert_eq!(w.patch, None, "a new patch next time");
        assert!(w.near_bad_patch(50.0, 5.0, 100.0));
        assert!(!w.near_bad_patch(500.0, 5.0, 100.0));
        for i in 0..20 {
            w.patch = Some([i as f32 * 1000.0, 0.0]);
            w.add(plot(i as f32 * 1000.0, 10));
            w.stolen_at([i as f32 * 1000.0, 0.0, 0.0]);
        }
        assert_eq!(w.bad_patches.len(), MAX_BAD_PATCHES);
    }

    #[test]
    fn old_plots_are_forgotten() {
        let mut w = WildPlots {
            patch: Some([1.0, 1.0]),
            ..WildPlots::default()
        };
        w.add(plot(0.0, 100));
        w.add(plot(5.0, 100 + MAX_PLOT_AGE_SECS));
        assert_eq!(w.prune(100 + MAX_PLOT_AGE_SECS), 1);
        assert_eq!(w.plots.len(), 1);
        assert!(w.patch.is_some());
        assert_eq!(w.prune(200 + 2 * MAX_PLOT_AGE_SECS), 1);
        assert_eq!(w.patch, None, "nothing grows there any more");
    }
}
