//! The graph foundation: identifier interning and CSR adjacency.
//!
//! [`intern`] assigns stable insertion-order indices to names; [`csr`] lays the
//! filtered dependency graph out in cache-friendly struct-of-arrays form (ad-6)
//! and derives the forward and reverse views every solver phase reads.

pub mod csr;
pub mod intern;
