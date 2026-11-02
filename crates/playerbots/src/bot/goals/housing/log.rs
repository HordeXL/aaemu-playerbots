//! What the bot noticed about housing, kept in the bot state next to `houses`.

use aa_proto::sc_activity::ActivityEvent;
use smallvec::SmallVec;

/// The answer to `CSConstructHouseTax`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    /// Design the quote is for.
    pub design: u32,
    /// Copper to pay in total (the server takes tax certificates, one per 10 000 copper, rounded
    /// up).
    pub total: i32,
    /// Bot clock (ms) when it arrived.
    pub at_ms: u64,
}

/// See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HouseLog {
    /// The latest tax quote.
    pub quote: Option<Quote>,
    /// Object ids of the bot's houses by transport id (`BotState::houses` has no object id, and
    /// the build command needs it to target the house).
    pub objs: SmallVec<[(u16, u32); 2]>,
    /// House id the hub's map marker was set for (0 = none).
    pub marker: u32,
}

impl HouseLog {
    /// Updates the log from one event; `me` is the bot's character id.
    pub fn apply(&mut self, ev: &ActivityEvent, now_ms: u64, me: u32) {
        match ev {
            ActivityEvent::ConstructHouseTax { design, total, .. } => {
                self.quote = Some(Quote {
                    design: *design,
                    total: *total,
                    at_ms: now_ms,
                });
            }
            ActivityEvent::MyHouse(h) => self.remember(h.tl, h.obj),
            ActivityEvent::LoginCharInfoHouse { owner_id, house } if *owner_id == me && me != 0 => {
                self.remember(house.tl, house.obj);
            }
            ActivityEvent::MyHouseRemoved { tl } => self.objs.retain(|o| o.0 != *tl),
            _ => {}
        }
    }

    fn remember(&mut self, tl: u16, obj: u32) {
        if let Some(o) = self.objs.iter_mut().find(|o| o.0 == tl) {
            o.1 = obj;
        } else if self.objs.len() < 4 {
            self.objs.push((tl, obj));
        }
    }

    /// The object id of the house with transport id `tl`.
    pub fn obj_of(&self, tl: u16) -> Option<u32> {
        self.objs.iter().find(|o| o.0 == tl).map(|o| o.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::sc_activity::HouseInfo;

    fn house(tl: u16, obj: u32) -> Box<HouseInfo> {
        Box::new(HouseInfo {
            tl,
            id: 9,
            obj,
            template: 267,
            model: 0,
            co_owner: 0,
            owner: 5,
            owner_name: "Me".into(),
            account: 1,
            permission: 0,
            all_action: 4,
            cur_action: 0,
            tax: 0,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            name: "".into(),
            allow_recover: true,
            sell_price: 0,
            sell_to_id: 0,
            sell_to_name: "".into(),
        })
    }

    #[test]
    fn quotes_and_house_objects_are_remembered() {
        let mut l = HouseLog::default();
        l.apply(
            &ActivityEvent::ConstructHouseTax {
                design: 267,
                heavy_count: 0,
                normal_count: 0,
                heavy: false,
                base: 50_000,
                deposit: 100_000,
                total: 150_000,
            },
            500,
            5,
        );
        assert_eq!(
            l.quote,
            Some(Quote {
                design: 267,
                total: 150_000,
                at_ms: 500
            })
        );
        l.apply(&ActivityEvent::MyHouse(house(12, 700)), 600, 5);
        assert_eq!(l.obj_of(12), Some(700));
        // other characters' houses (the login character list) are not ours
        l.apply(
            &ActivityEvent::LoginCharInfoHouse {
                owner_id: 6,
                house: house(13, 701),
            },
            600,
            5,
        );
        assert_eq!(l.obj_of(13), None);
        l.apply(
            &ActivityEvent::LoginCharInfoHouse {
                owner_id: 5,
                house: house(14, 702),
            },
            600,
            5,
        );
        assert_eq!(l.obj_of(14), Some(702));
        l.apply(&ActivityEvent::MyHouseRemoved { tl: 12 }, 700, 5);
        assert_eq!(l.obj_of(12), None);
        assert_eq!(l.objs.len(), 1);
    }
}
