// SPDX-License-Identifier: AGPL-3.0-or-later
// Synthetic fixture: every shape of `use` tree.

use std::collections::HashMap;
use crate::model::{Point, shapes::{Circle, Square as Sq}};
use super::util::*;
use self::local::thing;
pub use crate::api::Client;
pub(crate) use crate::api::{self, Server};
extern crate alloc as heap;
