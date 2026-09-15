/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `sys/timeb.h`

use crate::dyld::FunctionExports;
use crate::libc::time::{guest_wall_clock_now, time_t};
use crate::mem::{MutPtr, SafeRead};
use crate::{export_c_func, Environment};
use std::time::SystemTime;

#[allow(non_camel_case_types)]
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
struct timeb {
    /// Seconds since the epoch
    time: time_t,
    /// Up to 1000 milliseconds of more-precise interval
    millitm: u16,
    /// The local time zone
    timezone: i16,
    /// Daylight Saving time flag
    dstflag: i16,
}
unsafe impl SafeRead for timeb {}

fn ftime(env: &mut Environment, tb: MutPtr<timeb>) -> i32 {
    // [扫描修 2026-09-15] 接入时间旅行偏移:改读统一的虚拟墙钟(偏移为 0 时与原先一致)。
    let epoch_duration = guest_wall_clock_now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let time64 = epoch_duration.as_secs();
    let time = time64 as time_t;
    let millitm: u16 = (epoch_duration.as_millis() % 1000) as u16;

    env.mem.write(
        tb,
        timeb {
            time,
            millitm,
            timezone: 0, // TODO
            dstflag: 0,  // TODO
        },
    );
    0 // Success (always)
}

pub const FUNCTIONS: FunctionExports = &[export_c_func!(ftime(_))];
