//! The client-side representation cache (SPEC §8.3, DESIGN "The client-side cache").

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the decorator over the store lands next")
)]

mod store;
