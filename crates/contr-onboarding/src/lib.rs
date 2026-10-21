//! First-run setup: the engine the daemon manages, the fixes its checks offer, and logging out when a fix needs a new
//! session.

mod model;
mod page;

pub use page::{OnboardingCmd, OnboardingMsg, OnboardingOutput, OnboardingPage};
