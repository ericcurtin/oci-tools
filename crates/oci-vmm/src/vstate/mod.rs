// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
// Ported into oci-vmm from Firecracker (src/vmm/src/vstate/mod.rs), trimmed of
// metrics/snapshot/templates.

//! KVM virtual machine state: the VM file descriptor, its vCPUs, and the
//! MSI-X interrupt plumbing shared by the PCI transport.

pub mod interrupts;
pub mod vcpu;
pub mod vm;
