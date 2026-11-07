//! The `wild` section of the journal: plots a bot planted out in the open (owned by the wild-farm
//! feature). The server lists a character's crops on the public farms (`CSRequestCommonFarmList`)
//! but nothing else, so a bot that restarts finds its wild plots again only through this.

use serde::{Deserialize, Serialize};

/// Plots kept at most.
pub const MAX_WILD_PLOTS: usize = 32;

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
}

/// The bot's wild plots.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WildPlots {
    /// The plots, oldest first, at most [`MAX_WILD_PLOTS`].
    pub plots: Vec<WildPlot>,
}

impl WildPlots {
    /// Records a plot; the oldest goes when the section is full.
    pub fn add(&mut self, plot: WildPlot) {
        if self.plots.len() >= MAX_WILD_PLOTS {
            self.plots.remove(0);
        }
        self.plots.push(plot);
    }
}
