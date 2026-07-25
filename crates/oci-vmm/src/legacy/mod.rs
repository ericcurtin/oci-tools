// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/legacy/mod.rs), trimmed of metrics/snapshot/ACPI.

//! Legacy (port I/O) devices: the 16550 UART serial console and the i8042
//! PS/2 controller stub used for the classic `reboot=k` guest shutdown pulse.

pub mod i8042;
pub mod serial;

pub use self::i8042::I8042Device;
pub use self::serial::{EventFdTrigger, SerialDevice};
