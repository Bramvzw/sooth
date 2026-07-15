//! Analysis passes over parsed reports. Each analyzer is strictly its own
//! pass — flaky detection repeats in a fixed order, order-dependence (v0.3)
//! shuffles, and the two never run in one analysis (see `DECISIONS.md`).

pub mod flaky;
