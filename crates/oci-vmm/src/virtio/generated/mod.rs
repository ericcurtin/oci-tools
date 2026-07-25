// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/generated/mod.rs), trimmed of metrics/snapshot/MMIO.

//! Bindgen-generated constants from the Linux virtio UAPI headers.
//! Only the modules needed by the queue/device/transport/block code are kept.

#![allow(clippy::all)]
#![allow(missing_docs)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

pub mod virtio_blk;
pub mod virtio_config;
pub mod virtio_ids;
pub mod virtio_net;
pub mod virtio_ring;
