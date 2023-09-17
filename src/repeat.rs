use std::time::{Duration, Instant};

use wayrs_utils::keyboard::{xkb, RepeatInfo};

use crate::Action;

pub enum RepeatState {
    None,
    Delay {
        info: RepeatInfo,
        delay_will_end: Instant,
        action: Action,
        key: xkb::Keycode,
    },
    Repeat {
        info: RepeatInfo,
        next_repeat: Instant,
        action: Action,
        key: xkb::Keycode,
    },
}

impl RepeatState {
    pub fn key(&self) -> Option<xkb::Keycode> {
        match self {
            RepeatState::None => None,
            RepeatState::Delay { key, .. } => Some(*key),
            RepeatState::Repeat { key, .. } => Some(*key),
        }
    }

    pub fn timeout(&self) -> Option<Duration> {
        match &self {
            Self::None => None,
            Self::Delay {
                delay_will_end: instant,
                ..
            }
            | Self::Repeat {
                next_repeat: instant,
                ..
            } => Some(instant.saturating_duration_since(Instant::now())),
        }
    }

    pub fn tick(&mut self) -> Option<Action> {
        match self {
            Self::None => None,
            Self::Delay {
                info,
                delay_will_end,
                action,
                key,
            } => {
                if delay_will_end
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    == 0
                {
                    let action = *action;
                    *self = Self::Repeat {
                        info: *info,
                        next_repeat: *delay_will_end + info.interval,
                        action,
                        key: *key,
                    };
                    Some(action)
                } else {
                    None
                }
            }
            Self::Repeat {
                info,
                next_repeat,
                action,
                ..
            } => {
                if next_repeat
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    == 0
                {
                    *next_repeat += info.interval;
                    Some(*action)
                } else {
                    None
                }
            }
        }
    }
}
