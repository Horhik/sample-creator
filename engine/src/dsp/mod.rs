//! Offline processing of a region held in memory (regions are seconds to a few
//! minutes, never whole recordings).

pub mod fx;
pub mod speed;
pub mod splice;

pub use speed::change_speed;
pub use splice::splice;
