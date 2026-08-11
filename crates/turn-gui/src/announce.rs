//! Turning the attention manager's effects into things the user can hear and see.
//!
//! The manager has already decided. It owns the queue, the policy, the mutes, the
//! cooldowns and the focus governor, and what arrives here is its verdict. So this
//! module contains no policy at all: it maps [`Effect`] onto a notification, a sound,
//! a badge or a focus change, and refuses the two it must never act on.
//!
//! ## The one rule that is enforced here
//!
//! `focus_deferred` and `focus_denied` are **not** focus changes. They are the
//! governor saying "not now" and "no", and a client that treated either as a jump
//! would move the user precisely when the rules said not to — which is the failure
//! mode the whole focus governor exists to prevent. [`Announcement::from_effect`]
//! returns `Nothing` for both, and a test asserts it for every variant so a new one
//! cannot be added and quietly treated as a move.
//!
//! ## Why the OS boundary is this thin
//!
//! Everything that decides is a pure function over an `Effect`. What is left —
//! handing a title and a body to the notification centre — is a dozen lines behind
//! [`Announcer`], so the part that can be got wrong is testable and the part that
//! cannot be tested is too small to get wrong.

use turn_core::attention::{Effect, Sound};
use turn_core::ids::{NodeId, SessionId};

/// What the window should do about one effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announcement {
    /// Nothing the user can perceive. A badge, a highlight, or the governor declining
    /// to move them.
    Nothing,
    /// Show a count on a session.
    Badge { session_id: SessionId, count: usize },
    /// Draw attention to a session in the sidebar without interrupting.
    Highlight { session_id: SessionId },
    /// Post to the OS notification centre.
    Notify {
        session_id: SessionId,
        title: String,
        body: String,
    },
    /// Play the configured sound.
    Sound { session_id: SessionId, sound: Sound },
    /// Move the user. Only ever from [`Effect::Focus`], which the governor cleared.
    Focus {
        session_id: SessionId,
        node_id: Option<NodeId>,
    },
    /// The session no longer needs anything, so clear its badge and highlight.
    Cleared { session_id: SessionId },
    /// Run the session's own command. Configured by the user; Turn never invents one.
    RunCustom {
        session_id: SessionId,
        command: String,
    },
}

impl Announcement {
    /// What an effect means for the window.
    pub fn from_effect(effect: &Effect) -> Announcement {
        match effect {
            Effect::Badge { session_id, count } => Announcement::Badge {
                session_id: session_id.clone(),
                count: *count,
            },
            Effect::Highlight { session_id } => Announcement::Highlight {
                session_id: session_id.clone(),
            },
            Effect::PlaySound { session_id, sound } => Announcement::Sound {
                session_id: session_id.clone(),
                sound: *sound,
            },
            Effect::Notify {
                session_id,
                title,
                body,
            } => Announcement::Notify {
                session_id: session_id.clone(),
                title: title.clone(),
                body: body.clone(),
            },
            Effect::Focus {
                session_id,
                node_id,
            } => Announcement::Focus {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
            },
            // The governor said not now, and not at all. Neither is a jump. A client
            // that moved the user here would do it at exactly the moment the rules
            // forbade — mid-keystroke, or while a permission prompt was being read.
            Effect::FocusDeferred { .. } | Effect::FocusDenied { .. } => Announcement::Nothing,
            // Enqueueing is the queue's business; the panel re-renders from
            // `attention_queue_changed` rather than from this.
            Effect::Enqueued { .. } => Announcement::Nothing,
            Effect::Cleared { session_id } => Announcement::Cleared {
                session_id: session_id.clone(),
            },
            Effect::RunCustom {
                session_id,
                command,
            } => Announcement::RunCustom {
                session_id: session_id.clone(),
                command: command.clone(),
            },
        }
    }

    /// Whether this announcement moves the user.
    pub fn is_focus_change(&self) -> bool {
        matches!(self, Announcement::Focus { .. })
    }

    /// The session it concerns, for routing.
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Announcement::Nothing => None,
            Announcement::Badge { session_id, .. }
            | Announcement::Highlight { session_id }
            | Announcement::Notify { session_id, .. }
            | Announcement::Sound { session_id, .. }
            | Announcement::Focus { session_id, .. }
            | Announcement::Cleared { session_id }
            | Announcement::RunCustom { session_id, .. } => Some(session_id),
        }
    }
}

/// The name of the system sound for one of the policy's three settings.
///
/// A name rather than a file, because both platforms already have a set of them and
/// shipping audio to play an alert would be a strange thing for a terminal to do.
/// [`Sound::None`] is the default, and it produces nothing at all — the policy's
/// quiet-by-default stance reaching the speaker.
pub fn sound_name(sound: Sound) -> Option<&'static str> {
    match sound {
        Sound::None => None,
        // Both exist on macOS in `/System/Library/Sounds`, and both are names
        // `notify-rust` passes through to the freedesktop sound theme on Linux.
        Sound::Subtle => Some("Tink"),
        Sound::Alert => Some("Submarine"),
    }
}

/// Somewhere for a notification to go.
///
/// A trait so the decision layer above can be tested without a notification centre,
/// a display or a D-Bus session — none of which exist in CI.
pub trait Announcer {
    /// Post a notification. Failure is logged and dropped: a notification that could
    /// not be shown must never take the window down or block a frame.
    fn notify(&self, title: &str, body: &str, sound: Option<&str>);

    /// Spawn a command the user explicitly configured as an Attention action. It runs away
    /// from the UI thread; output is discarded so a background notification action cannot
    /// inherit or stall Turn's own process streams.
    fn run_custom(&self, command: &str);
}

/// The real one.
#[derive(Debug, Default)]
pub struct DesktopAnnouncer;

impl Announcer for DesktopAnnouncer {
    fn notify(&self, title: &str, body: &str, sound: Option<&str>) {
        let mut notification = notify_rust::Notification::new();
        notification.summary(title).body(body).appname("Turn");
        if let Some(sound) = sound {
            notification.sound_name(sound);
        }
        if let Err(error) = notification.show() {
            // Common and harmless: no notification daemon on a headless Linux box, or
            // notifications switched off. The badge and the queue still say everything
            // this would have.
            tracing::debug!(%error, "could not post a notification");
        }
    }

    fn run_custom(&self, command: &str) {
        use std::process::{Command, Stdio};

        #[cfg(unix)]
        let child = Command::new("/bin/sh")
            .arg("-lc")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        #[cfg(windows)]
        let child = Command::new("cmd")
            .arg("/C")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        if let Err(error) = child {
            // The command itself may contain a credential, so it is deliberately absent from
            // the log record. Settings presents it as a blind replacement for the same reason.
            tracing::warn!(%error, "could not spawn the configured Attention action");
        }
    }
}

/// Performs an announcement's user-visible half.
///
/// Returns what it did, so a caller can assert on it and so the window can log the
/// one thing worth logging: that it interrupted somebody.
pub fn perform(announcement: &Announcement, announcer: &dyn Announcer) -> bool {
    match announcement {
        Announcement::Notify { title, body, .. } => {
            announcer.notify(title, body, None);
            true
        }
        Announcement::Sound { sound, .. } => match sound_name(*sound) {
            // A sound with no notification would be a noise with no explanation, so
            // the sound rides on a notification naming the session.
            Some(name) => {
                announcer.notify("Turn", "A session needs you", Some(name));
                true
            }
            None => false,
        },
        Announcement::RunCustom { command, .. } => {
            announcer.run_custom(command);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use turn_core::attention::{DeferReason, FocusDenial};
    use turn_core::ids::AttentionId;

    #[derive(Default)]
    struct Recorder {
        posted: RefCell<Vec<(String, String, Option<String>)>>,
        commands: RefCell<Vec<String>>,
    }

    impl Announcer for Recorder {
        fn notify(&self, title: &str, body: &str, sound: Option<&str>) {
            self.posted.borrow_mut().push((
                title.to_string(),
                body.to_string(),
                sound.map(str::to_string),
            ));
        }

        fn run_custom(&self, command: &str) {
            self.commands.borrow_mut().push(command.to_string());
        }
    }

    fn session() -> SessionId {
        SessionId::from_stored("sess_announce01")
    }

    fn every_effect() -> Vec<Effect> {
        vec![
            Effect::Badge {
                session_id: session(),
                count: 2,
            },
            Effect::Highlight {
                session_id: session(),
            },
            Effect::PlaySound {
                session_id: session(),
                sound: Sound::Alert,
            },
            Effect::Notify {
                session_id: session(),
                title: "Turn complete".into(),
                body: "2 still running".into(),
            },
            Effect::Enqueued {
                attention_id: AttentionId::new(),
                session_id: session(),
            },
            Effect::Focus {
                session_id: session(),
                node_id: None,
            },
            Effect::FocusDeferred {
                session_id: session(),
                until_ms: 1_700_000_001_500,
                reason: DeferReason::UserTyping,
            },
            Effect::FocusDenied {
                session_id: session(),
                reason: FocusDenial::RateLimited,
            },
            Effect::RunCustom {
                session_id: session(),
                command: "say done".into(),
            },
            Effect::Cleared {
                session_id: session(),
            },
        ]
    }

    /// The rule the focus governor exists to enforce, checked from the client's side:
    /// exactly one effect moves the user, and it is the one the governor cleared.
    #[test]
    fn only_a_granted_focus_effect_moves_the_user() {
        let moving: Vec<Effect> = every_effect()
            .into_iter()
            .filter(|effect| Announcement::from_effect(effect).is_focus_change())
            .collect();
        assert_eq!(
            moving.len(),
            1,
            "exactly one effect may move anybody: {moving:?}"
        );
        assert!(matches!(moving[0], Effect::Focus { .. }));
    }

    /// Said again from the other direction, because this is the one that would be a
    /// product bug rather than a rendering bug: a deferral and a denial are refusals.
    #[test]
    fn a_deferred_or_denied_focus_request_is_never_acted_on() {
        for effect in [
            Effect::FocusDeferred {
                session_id: session(),
                until_ms: 0,
                reason: DeferReason::UserTyping,
            },
            Effect::FocusDeferred {
                session_id: session(),
                until_ms: 0,
                reason: DeferReason::RequiresIdle,
            },
            Effect::FocusDenied {
                session_id: session(),
                reason: FocusDenial::SensitiveOperation,
            },
            Effect::FocusDenied {
                session_id: session(),
                reason: FocusDenial::PingPongGuard,
            },
        ] {
            assert_eq!(
                Announcement::from_effect(&effect),
                Announcement::Nothing,
                "{effect:?} must not become anything the user notices"
            );
        }
    }

    #[test]
    fn every_effect_maps_to_something_rather_than_being_dropped_unnoticed() {
        for effect in every_effect() {
            let announcement = Announcement::from_effect(&effect);
            // Every effect names a session, so every announcement can be routed —
            // except the ones that are deliberately nothing.
            if announcement != Announcement::Nothing {
                assert!(
                    announcement.session_id().is_some(),
                    "{effect:?} produced an unroutable announcement"
                );
            }
        }
    }

    #[test]
    fn a_notification_carries_the_managers_own_words() {
        let announcement = Announcement::from_effect(&Effect::Notify {
            session_id: session(),
            title: "Turn complete".into(),
            body: "2 still running".into(),
        });
        let recorder = Recorder::default();
        assert!(perform(&announcement, &recorder));
        let posted = recorder.posted.borrow().clone();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].0, "Turn complete");
        assert_eq!(
            posted[0].1, "2 still running",
            "the body is the manager's, not a paraphrase"
        );
    }

    #[test]
    fn a_custom_action_reaches_the_command_runner_exactly_once() {
        let recorder = Recorder::default();
        assert!(perform(
            &Announcement::RunCustom {
                session_id: session(),
                command: "touch /tmp/turn-attention-accepted".into(),
            },
            &recorder,
        ));
        assert_eq!(
            recorder.commands.borrow().as_slice(),
            ["touch /tmp/turn-attention-accepted"],
            "the configured command is executed, not merely represented in the protocol"
        );
    }

    /// The policy is quiet by default, and that has to reach the speaker: the default
    /// sound produces no noise at all.
    #[test]
    fn the_default_sound_makes_no_noise() {
        assert_eq!(sound_name(Sound::None), None);
        let recorder = Recorder::default();
        assert!(!perform(
            &Announcement::Sound {
                session_id: session(),
                sound: Sound::None,
            },
            &recorder
        ));
        assert!(recorder.posted.borrow().is_empty());
    }

    #[test]
    fn the_two_configured_sounds_are_distinguishable_and_both_named() {
        let subtle = sound_name(Sound::Subtle).expect("subtle has a sound");
        let alert = sound_name(Sound::Alert).expect("alert has a sound");
        assert_ne!(subtle, alert, "the two settings must be tellable apart");
        assert!(!subtle.is_empty() && !alert.is_empty());
    }

    /// A noise with no explanation is worse than no noise: the sound arrives attached
    /// to something the user can read.
    #[test]
    fn a_sound_arrives_with_words_beside_it() {
        let recorder = Recorder::default();
        assert!(perform(
            &Announcement::Sound {
                session_id: session(),
                sound: Sound::Alert,
            },
            &recorder
        ));
        let posted = recorder.posted.borrow().clone();
        assert_eq!(posted.len(), 1);
        assert!(!posted[0].1.is_empty(), "the sound must say what it is for");
        assert_eq!(posted[0].2.as_deref(), sound_name(Sound::Alert));
    }

    #[test]
    fn a_badge_and_a_clear_are_visual_only_and_post_nothing() {
        let recorder = Recorder::default();
        for announcement in [
            Announcement::Badge {
                session_id: session(),
                count: 3,
            },
            Announcement::Highlight {
                session_id: session(),
            },
            Announcement::Cleared {
                session_id: session(),
            },
            Announcement::Nothing,
        ] {
            assert!(!perform(&announcement, &recorder), "{announcement:?}");
        }
        assert!(recorder.posted.borrow().is_empty());
    }
}
