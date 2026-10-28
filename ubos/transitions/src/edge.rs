// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! The edges a state declares: the enum it returns, with one variant per
//! edge, and the same edges as data so that a machine can be checked.

/// One declared transition: the variant that triggers it and the state
/// it leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub trigger: &'static str,
    pub to: &'static str,
}

/// The edges of one state, declared by [`successors!`](crate::successors).
pub trait Edges {
    const EDGES: &'static [Edge];
}

/// Declare the edges of one state as an enum with one variant per edge.
///
/// The variant name is the trigger and its payload is the state the edge
/// leads to. Several variants may lead to the same state, and a state
/// with no edge of its own declares an empty enum. The enum takes plain
/// type parameters, which its payload types may use.
///
/// ```
/// # use transitions::{successors, Named};
/// # struct Work; struct Halt;
/// # impl Named for Work { const NAME: &'static str = "Work"; }
/// # impl Named for Halt { const NAME: &'static str = "Halt"; }
/// successors! {
///     /// What the idle state may lead to.
///     pub enum IdleNext { Go(Work), Stop(Halt) }
/// }
/// ```
#[macro_export]
macro_rules! successors {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident $(<$($gen:ident),+>)? {
            $( $(#[$variant_meta:meta])* $variant:ident($dest:ty) ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name $(<$($gen),+>)? {
            $( $(#[$variant_meta])* $variant($dest) ),*
        }

        impl $(<$($gen),+>)? $crate::Edges for $name $(<$($gen),+>)? {
            const EDGES: &'static [$crate::Edge] = &[
                $( $crate::Edge {
                    trigger: stringify!($variant),
                    to: <$dest as $crate::Named>::NAME,
                } ),*
            ];
        }

        impl<__Target $(, $($gen),+)?> $crate::Lift<__Target>
            for $name $(<$($gen),+>)?
        where
            __Target: Sized $(+ From<$dest>)*,
        {
            fn lift(self) -> __Target {
                match self {
                    $( Self::$variant(state) => __Target::from(state), )*
                }
            }
        }
    };
}
