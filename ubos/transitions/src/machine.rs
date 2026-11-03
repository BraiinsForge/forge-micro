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

//! The traits of a machine: its states, the sum type over them that
//! [`machine!`](crate::machine) declares, and the graph of their declared
//! edges that the check reads.

use core::future::Future;

use crate::edge::{Edge, Edges};

#[cfg(test)]
#[path = "tests/machine.rs"]
mod tests;

/// The name of a state in reports of its edges.
pub trait Named {
    const NAME: &'static str;
}

/// Turns a state's `Next` into the machine's sum type of states.
///
/// [`successors!`](crate::successors) writes the impls, and
/// [`machine!`](crate::machine) calls them.
#[doc(hidden)]
pub trait Lift<Target> {
    fn lift(self) -> Target;
}

/// One state of a machine whose states borrow a context `C` to do their
/// work.
pub trait State<C>: Named + Sized {
    /// Every state this one may lead to.
    type Next: Edges;

    /// Do the state's work against the context and return the state it
    /// leads to.
    fn run(self, context: &mut C) -> impl Future<Output = Self::Next>;
}

/// A state of the machine in its graph: its name and its declared
/// edges.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub name: &'static str,
    pub edges: &'static [Edge],
}

/// The [`Node`] of state `S` in a machine over context `C`, for its
/// [`Graph`], which [`machine!`](crate::machine) writes.
#[doc(hidden)]
#[must_use]
pub const fn node<C, S: State<C>>() -> Node {
    Node {
        name: S::NAME,
        edges: <S::Next as Edges>::EDGES,
    }
}

/// The graph of a machine over context `C`: a node for every state, and
/// the transitions that apply to every state.
///
/// The sum type implements it, and [`check`](crate::check) reads it.
pub trait Graph<C> {
    const NODES: &'static [Node];
    /// A machine without such a transition names an empty enum.
    type Anywhere: Edges;
}

/// A state machine over context `C`: the sum type over its states.
///
/// [`machine!`](crate::machine) implements it, so the list of states is
/// written once.
pub trait Machine<C>: Graph<C> + Sized {
    /// Run the current state against `context` and return the state it
    /// leads to.
    fn run(self, context: &mut C) -> impl Future<Output = Self>;
}

/// Declare a machine: the sum type over its states, with one variant per
/// state.
///
/// The macro writes the rest from the one list, so a state added to the
/// list is added to the machine everywhere at once.
///
/// # Syntax
///
/// ```text
/// machine! {
///     $(#[meta])*
///     $vis enum $Name<$G, ...> {
///         $(#[meta])* $Variant($State<$G, ...>),
///         ...
///     }
///     anywhere = $Anywhere;
/// }
/// ```
///
/// - The attributes and the visibility go to the enum as written, and the
///   attributes of a variant go to that variant. A derive written here
///   applies to the sum type, so every state must meet its bounds.
/// - Each variant wraps one state, and each state appears once. The
///   variant name is the state's name in reports of its edges.
/// - The type parameters are optional on the enum and on each state. A
///   state names the parameters of the enum, and may name only some of
///   them, as `Work<T>` does in the example below.
/// - `anywhere` is the [`Graph::Anywhere`] of the machine, a type that
///   implements [`Edges`]. A machine with no such transition names an
///   empty enum.
///
/// # What it writes
///
/// | Item | For | Purpose |
/// |------|-----|---------|
/// | the enum | the machine | Holds the state the machine is in. |
/// | [`Named`] | every state | Gives the state the name of its variant. |
/// | [`From`] | every state | Wraps the state into its variant of the enum. |
/// | [`Graph`] | the enum | Lists a [`Node`] for every state, in variant order, and sets `Anywhere`. |
/// | [`Machine`] | the enum | Runs the current state and lifts its `Next` into the enum. |
///
/// The [`Graph`] and [`Machine`] impls are generic over a context `C`, and
/// hold for every `C` that all the states implement [`State<C>`] for. The
/// machine is therefore tied to its context by its states, and the macro
/// names no context itself.
///
/// The [`Machine`] impl also requires that the `Next` of every state is
/// [`Lift`] into the enum. [`successors!`](crate::successors) writes that impl for an edge
/// enum when the enum has a [`From`] for every state it names, so an edge
/// to a state outside the list makes the [`Machine`] impl unavailable.
///
/// # Parameters and bounds
///
/// The enum and its states take plain type parameters, without bounds or
/// lifetimes. A bound that a state needs belongs on its [`State<C>`]
/// impl, and the generated impls carry it through as the requirement that
/// the state implements [`State<C>`].
///
/// A state belongs to one machine, because its [`Named`] impl is the
/// variant's name. Two machines over the same state write the impl twice,
/// which does not compile:
///
/// ```compile_fail
/// # use transitions::{machine, successors};
/// struct Idle;
/// successors! { enum Anywhere { Stop(Idle) } }
/// machine! { enum First { Idle(Idle) } anywhere = Anywhere; }
/// machine! { enum Second { Idle(Idle) } anywhere = Anywhere; }
/// ```
///
/// # Example
///
/// ```
/// # use transitions::{machine, successors};
/// struct Idle;
/// struct Work<T>(T);
/// successors! { enum Anywhere { Stop(Idle) } }
/// machine! {
///     /// Every state of the machine.
///     enum MachineState<T> {
///         Idle(Idle),
///         Work(Work<T>),
///     }
///     anywhere = Anywhere;
/// }
/// # let _ = (MachineState::<u8>::Idle(Idle), MachineState::Work(Work(0_u8)));
/// ```
#[macro_export]
macro_rules! machine {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident $(<$($gen:ident),+ $(,)?>)? {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident($state:ident $(<$($state_gen:ident),+ $(,)?>)?)
            ),+ $(,)?
        }
        anywhere = $anywhere:ty;
    ) => {
        $(#[$meta])*
        $vis enum $name $(<$($gen),+>)? {
            $(
                $(#[$variant_meta])*
                $variant($state $(<$($state_gen),+>)?)
            ),+
        }

        $crate::machine!(
            @impl [$($($gen),+)?] $name $anywhere;
            $($variant $state [$($($state_gen),+)?])+
        );
    };
    // The sum type's parameters travel as one bracketed token tree, so
    // that each state's impls can repeat them.
    (
        @impl $gens:tt $name:ident $anywhere:ty;
        $($variant:ident $state:ident $state_gens:tt)+
    ) => {
        $($crate::machine!(@state $gens $name $variant $state $state_gens);)+
        $crate::machine!(@graph $gens $name $anywhere; $($variant $state $state_gens)+);
    };
    (
        @state [$($gen:ident),*] $name:ident $variant:ident $state:ident
        [$($state_gen:ident),*]
    ) => {
        impl<$($state_gen),*> $crate::Named for $state<$($state_gen),*> {
            const NAME: &'static str = stringify!($variant);
        }

        impl<$($gen),*> ::core::convert::From<$state<$($state_gen),*>> for $name<$($gen),*> {
            fn from(state: $state<$($state_gen),*>) -> Self {
                Self::$variant(state)
            }
        }
    };
    (
        @graph [$($gen:ident),*] $name:ident $anywhere:ty;
        $($variant:ident $state:ident [$($state_gen:ident),*])+
    ) => {
        impl<__Context, $($gen),*> $crate::Graph<__Context> for $name<$($gen),*>
        where
            $($state<$($state_gen),*>: $crate::State<__Context>,)+
        {
            const NODES: &'static [$crate::Node] =
                &[$($crate::node::<__Context, $state<$($state_gen),*>>()),+];
            type Anywhere = $anywhere;
        }

        impl<__Context, $($gen),*> $crate::Machine<__Context> for $name<$($gen),*>
        where
            $(
                $state<$($state_gen),*>: $crate::State<__Context>,
                <$state<$($state_gen),*> as $crate::State<__Context>>::Next:
                    $crate::Lift<Self>,
            )+
        {
            async fn run(self, context: &mut __Context) -> Self {
                match self {
                    $(
                        Self::$variant(state) => $crate::Lift::lift(
                            $crate::State::run(state, context).await,
                        ),
                    )+
                }
            }
        }
    };
}
