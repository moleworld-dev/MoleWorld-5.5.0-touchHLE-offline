/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `net/if.h`

use crate::dyld::FunctionExports;
use crate::export_c_func;
use crate::mem::{ConstPtr, Ptr};
use crate::Environment;

// TODO: struct definition
#[allow(non_camel_case_types)]
struct if_nameindex {}

fn if_nameindex(_env: &mut Environment) -> ConstPtr<if_nameindex> {
    // TODO: implement
    Ptr::null()
}

/// `unsigned int if_nametoindex(const char *ifname)`.
/// Returns the interface index for a name, or 0 if none. Guest code that reads
/// the WiFi MAC address (e.g. the well-known Erica Sadun `+macaddress`, used by
/// Taomee's `taomeeUDID`) bails out early when this returns 0 and ends up with an
/// empty MAC string, which then trips a guest `assert([macaddress length]>0)` and
/// aborts the whole app. Report a valid nonzero index so it proceeds to the
/// `sysctl(NET_RT_IFLIST)` query, which we fake with a dummy MAC. Any nonzero
/// index works; the real value is meaningless in our offline, no-NIC environment.
fn if_nametoindex(_env: &mut Environment, ifname: ConstPtr<u8>) -> u32 {
    if ifname.is_null() {
        0
    } else {
        1
    }
}

pub const FUNCTIONS: FunctionExports =
    &[export_c_func!(if_nameindex()), export_c_func!(if_nametoindex(_))];
