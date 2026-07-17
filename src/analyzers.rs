//! Analysis passes over parsed reports — strictly one pass per analyzer,
//! never combined (see `ROADMAP.md`).

pub mod flaky;
pub mod history;
