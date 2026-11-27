// SPDX-License-Identifier: GPL-3.0-or-later

//! Original byte-built synthetic PE32+ fixtures for the non-executing module
//! binding planner. No host executable or proprietary image is inspected.

mod module_binding {
    use xenora_loader::export_table::ExportTableError;
    use xenora_loader::import_table::{ImportName, ImportTableError};
    use xenora_loader::module_binding::{
        BindingOutcome, IAT_SLOT_BYTES, IatSlotPlan, ModuleBindingError, ModuleBindingPlan,
        ModuleInspectionError, ResolvedBinding, UnresolvedBindingReason,
    };
    use xenora_loader::module_graph::{
        ExportKey, ForwarderParseError, ImportOutcome, ModuleGraph, PackageModule,
        UnresolvedImportReason,
    };

    const BASE: u64 = 0x1_4000_0000;
    const ROOT_BASE: u64 = 0x1_0000_0000;
    const LIB_BASE: u64 = 0x2_0000_0000;
    const OPTIONAL: usize = 0x98;
    const CODE_RVA: u32 = 0x1000;
    const DATA_RVA: u32 = 0x2000;
    const DATA_RAW: usize = 0x400;
    const DATA_LEN: usize = 0x4000;
    const DATA_LEN_U32: u32 = DATA_LEN as u32;
    const EXPORT_RVA: u32 = DATA_RVA;
    const EXPORT_LEN: u32 = 0x1000;
    const EXPORT_END: u32 = EXPORT_RVA + EXPORT_LEN;
    const EAT_RVA: u32 = EXPORT_RVA + 0x100;
    const NPT_RVA: u32 = EXPORT_RVA + 0x200;
    const EOT_RVA: u32 = EXPORT_RVA + 0x280;
    const EXPORT_STRINGS: u32 = EXPORT_RVA + 0x300;
    const IMPORT_RVA: u32 = EXPORT_END;
    const IMPORT_LEN: u32 = 0x1000;
    const IMPORT_END: u32 = IMPORT_RVA + IMPORT_LEN;

    enum ImportSpec {
        Named {
            module: String,
            symbol: String,
        },
        Ordinal {
            module: String,
            ordinal: u16,
        },
        /// A descriptor with a missing DLL-name RVA. It preserves the imports
        /// before it and then makes the ordinary import table malformed.
        BrokenDescriptor,
    }

    fn named(module: &str, symbol: &str) -> ImportSpec {
        ImportSpec::Named {
            module: module.to_owned(),
            symbol: symbol.to_owned(),
        }
    }

    fn ordinal(module: &str, ordinal: u16) -> ImportSpec {
        ImportSpec::Ordinal {
            module: module.to_owned(),
            ordinal,
        }
    }

    enum Target {
        Empty,
        Export,
        Forward(String),
    }

    struct ExportSlot {
        target: Target,
        names: Vec<String>,
    }

    struct ExportSpec {
        base: u32,
        module_name: String,
        slots: Vec<ExportSlot>,
    }

    fn name_list(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn slot(target: Target, names: &[&str]) -> ExportSlot {
        ExportSlot {
            target,
            names: name_list(names),
        }
    }

    fn export(module_name: &str, exported_names: &[&str]) -> ExportSpec {
        ExportSpec {
            base: 1,
            module_name: module_name.to_owned(),
            slots: vec![slot(Target::Export, exported_names)],
        }
    }

    struct TestImage {
        bytes: Vec<u8>,
        /// First-slot RVAs of valid one-symbol import descriptors, in input order.
        import_slots: Vec<u32>,
    }

    fn put(pe: &mut [u8], at: usize, bytes: &[u8]) {
        pe[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn put16(pe: &mut [u8], at: usize, value: u16) {
        put(pe, at, &value.to_le_bytes());
    }

    fn put32(pe: &mut [u8], at: usize, value: u32) {
        put(pe, at, &value.to_le_bytes());
    }

    fn data_offset(rva: u32) -> usize {
        DATA_RAW + (rva - DATA_RVA) as usize
    }

    fn put32_rva(pe: &mut [u8], rva: u32, value: u32) {
        put32(pe, data_offset(rva), value);
    }

    fn allocate(cursor: &mut u32, len: usize, end: u32) -> u32 {
        let start = *cursor;
        let len = u32::try_from(len).unwrap();
        let next = start.checked_add(len).unwrap();
        *cursor = next.next_multiple_of(8);
        assert!(*cursor <= end, "fixture data does not fit in its region");
        start
    }

    fn write_string(pe: &mut [u8], cursor: &mut u32, end: u32, text: &str) -> u32 {
        let rva = allocate(cursor, text.len() + 1, end);
        put(pe, data_offset(rva), text.as_bytes());
        rva
    }

    fn write_exports(pe: &mut [u8], exports: &ExportSpec) {
        put32(pe, OPTIONAL + 112, EXPORT_RVA);
        put32(pe, OPTIONAL + 116, EXPORT_LEN);

        let mut cursor = EXPORT_STRINGS;
        let module = write_string(pe, &mut cursor, EXPORT_END, &exports.module_name);
        let mut named = Vec::new();
        for (index, export_slot) in exports.slots.iter().enumerate() {
            for name in &export_slot.names {
                named.push((name.clone(), index));
            }
        }
        named.sort();
        for (position, (name, index)) in named.iter().enumerate() {
            let name_rva = write_string(pe, &mut cursor, EXPORT_END, name);
            put32_rva(pe, NPT_RVA + 4 * position as u32, name_rva);
            put16(
                pe,
                data_offset(EOT_RVA) + 2 * position,
                u16::try_from(*index).unwrap(),
            );
        }

        for (index, export_slot) in exports.slots.iter().enumerate() {
            let value = match &export_slot.target {
                Target::Empty => 0,
                Target::Export => CODE_RVA + 4 * index as u32,
                Target::Forward(text) => write_string(pe, &mut cursor, EXPORT_END, text),
            };
            put32_rva(pe, EAT_RVA + 4 * index as u32, value);
        }

        let name_count = named.len() as u32;
        put32_rva(pe, EXPORT_RVA + 12, module);
        put32_rva(pe, EXPORT_RVA + 16, exports.base);
        put32_rva(pe, EXPORT_RVA + 20, exports.slots.len() as u32);
        put32_rva(pe, EXPORT_RVA + 24, name_count);
        put32_rva(pe, EXPORT_RVA + 28, EAT_RVA);
        put32_rva(
            pe,
            EXPORT_RVA + 32,
            if name_count == 0 { 0 } else { NPT_RVA },
        );
        put32_rva(
            pe,
            EXPORT_RVA + 36,
            if name_count == 0 { 0 } else { EOT_RVA },
        );
    }

    fn write_imports(pe: &mut [u8], imports: &[ImportSpec]) -> Vec<u32> {
        put32(pe, OPTIONAL + 120, IMPORT_RVA);
        put32(pe, OPTIONAL + 124, IMPORT_LEN);
        let descriptor_len = 20 * (imports.len() as u32 + 1);
        let mut cursor = (IMPORT_RVA + descriptor_len).next_multiple_of(8);
        let mut slots = Vec::new();
        for (index, import) in imports.iter().enumerate() {
            let descriptor = data_offset(IMPORT_RVA) + 20 * index;
            if matches!(import, ImportSpec::BrokenDescriptor) {
                let lookup = allocate(&mut cursor, 16, IMPORT_END);
                let slot = allocate(&mut cursor, 16, IMPORT_END);
                put32(pe, descriptor, lookup);
                put32(pe, descriptor + 12, 0);
                put32(pe, descriptor + 16, slot);
                continue;
            }

            let (module, lookup_value) = match import {
                ImportSpec::Named { module, symbol } => {
                    let hint = allocate(&mut cursor, symbol.len() + 3, IMPORT_END);
                    put(pe, data_offset(hint) + 2, symbol.as_bytes());
                    (module, u64::from(hint))
                }
                ImportSpec::Ordinal { module, ordinal } => {
                    (module, (1u64 << 63) | u64::from(*ordinal))
                }
                ImportSpec::BrokenDescriptor => unreachable!(),
            };
            let module_rva = write_string(pe, &mut cursor, IMPORT_END, module);
            let lookup = allocate(&mut cursor, 16, IMPORT_END);
            let slot = allocate(&mut cursor, 16, IMPORT_END);
            put(pe, data_offset(lookup), &lookup_value.to_le_bytes());
            put(pe, data_offset(slot), &lookup_value.to_le_bytes());
            put32(pe, descriptor, lookup);
            put32(pe, descriptor + 12, module_rva);
            put32(pe, descriptor + 16, slot);
            slots.push(slot);
        }
        slots
    }

    fn build_image(dll: bool, imports: &[ImportSpec], exports: Option<&ExportSpec>) -> TestImage {
        let mut pe = vec![0; DATA_RAW + DATA_LEN];
        put(&mut pe, 0, b"MZ");
        put32(&mut pe, 0x3c, 0x80);
        put(&mut pe, 0x80, b"PE\0\0");
        put16(&mut pe, 0x84, 0x8664);
        put16(&mut pe, 0x86, 2);
        put16(&mut pe, 0x94, 240);
        put16(&mut pe, 0x96, if dll { 0x2000 } else { 0 });
        put16(&mut pe, OPTIONAL, 0x20b);
        put32(&mut pe, OPTIONAL + 16, if dll { 0 } else { CODE_RVA });
        put(&mut pe, OPTIONAL + 24, &BASE.to_le_bytes());
        put32(&mut pe, OPTIONAL + 32, 0x1000);
        put32(&mut pe, OPTIONAL + 36, 0x200);
        put32(&mut pe, OPTIONAL + 56, DATA_RVA + DATA_LEN_U32);
        put32(&mut pe, OPTIONAL + 60, 0x200);
        put32(&mut pe, OPTIONAL + 108, 16);
        for (index, (name, rva, raw, len, flags)) in [
            (b".text", CODE_RVA, 0x200usize, 0x200usize, 0x6000_0020u32),
            (b".data", DATA_RVA, DATA_RAW, DATA_LEN, 0xc000_0040u32),
        ]
        .into_iter()
        .enumerate()
        {
            let section = OPTIONAL + 240 + 40 * index;
            put(&mut pe, section, name);
            put32(&mut pe, section + 8, len as u32);
            put32(&mut pe, section + 12, rva);
            put32(&mut pe, section + 16, len as u32);
            put32(&mut pe, section + 20, raw as u32);
            put32(&mut pe, section + 36, flags);
        }
        pe[0x200] = 0xc3;

        let import_slots = if imports.is_empty() {
            Vec::new()
        } else {
            write_imports(&mut pe, imports)
        };
        if let Some(exports) = exports {
            write_exports(&mut pe, exports);
        }
        TestImage {
            bytes: pe,
            import_slots,
        }
    }

    fn package<'a>(items: &'a [(&str, TestImage)]) -> Vec<PackageModule<'a>> {
        items
            .iter()
            .map(|item| PackageModule {
                file_name: item.0,
                bytes: item.1.bytes.as_slice(),
            })
            .collect()
    }

    fn analyze(items: &[(&str, TestImage)], bases: &[u64]) -> ModuleBindingPlan {
        let package = package(items);
        let graph = ModuleGraph::build(&package).unwrap();
        ModuleBindingPlan::build(&graph, bases).unwrap()
    }

    fn resolved(slot: &IatSlotPlan) -> &ResolvedBinding {
        match &slot.outcome {
            BindingOutcome::Resolved(binding) => binding,
            outcome => panic!("slot is not resolved: {outcome:?}"),
        }
    }

    fn unresolved(slot: &IatSlotPlan) -> &UnresolvedBindingReason {
        match &slot.outcome {
            BindingOutcome::Unresolved(reason) => reason,
            outcome => panic!("slot unexpectedly resolved: {outcome:?}"),
        }
    }

    fn assert_overlap(slot: &IatSlotPlan, entries: usize) -> &ImportOutcome {
        match unresolved(slot) {
            UnresolvedBindingReason::OverlappingIatSlot {
                entries: actual,
                resolution,
            } => {
                assert_eq!(*actual, entries);
                resolution
            }
            reason => panic!("slot overlap was not diagnosed: {reason:?}"),
        }
    }

    fn import_reason(slot: &IatSlotPlan) -> &UnresolvedImportReason {
        match unresolved(slot) {
            UnresolvedBindingReason::Import(reason) => reason,
            reason => panic!("slot does not preserve a graph resolution failure: {reason:?}"),
        }
    }

    #[test]
    fn an_empty_package_has_an_empty_complete_plan() {
        let graph = ModuleGraph::build(&[]).unwrap();
        let plan = ModuleBindingPlan::build(&graph, &[]).unwrap();

        assert!(plan.inspection_is_complete());
        assert!(plan.slots().is_empty());
        assert!(plan.inspection_errors().is_empty());
    }

    #[test]
    fn named_import_plan_has_checked_slot_and_target_addresses() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["Start"])));
        let root = build_image(false, &[named("lib.dll", "Start")], None);
        let slot_rva = root.import_slots[0];
        let items = [("root.exe", root), ("lib.dll", library)];
        let bases = [ROOT_BASE, LIB_BASE];
        let plan = analyze(&items, &bases);

        assert!(plan.inspection_is_complete());
        assert_eq!(plan, analyze(&items, &bases));
        assert_eq!(plan.slots().len(), 1);
        let slot = &plan.slots()[0];
        assert_eq!(slot.consumer.index, 0);
        assert_eq!(slot.consumer.file_name, "root.exe");
        assert_eq!(slot.slot_rva, slot_rva);
        assert_eq!(slot.requested_module, "lib.dll");
        assert_eq!(slot.normalized_module, "lib.dll");
        assert_eq!(slot.requested_symbol, ImportName::Name("Start".to_owned()));
        assert!(slot.forwarder_chain.is_empty());

        let binding = resolved(slot);
        assert_eq!(binding.provider.index, 1);
        assert_eq!(binding.provider.file_name, "lib.dll");
        assert_eq!(binding.export, ExportKey::Name("Start".to_owned()));
        assert_eq!(binding.ordinal, 1);
        assert_eq!(binding.target_rva, CODE_RVA);
        assert_eq!(binding.consumer_base, ROOT_BASE);
        assert_eq!(binding.slot_address, ROOT_BASE + u64::from(slot_rva));
        assert_eq!(binding.provider_base, LIB_BASE);
        assert_eq!(binding.target_address, LIB_BASE + u64::from(CODE_RVA));
    }

    #[test]
    fn ordinal_import_uses_the_biased_export_ordinal() {
        let spec = ExportSpec {
            base: 11,
            module_name: "ordinal.dll".to_owned(),
            slots: vec![slot(Target::Export, &[]), slot(Target::Export, &[])],
        };
        let library = build_image(true, &[], Some(&spec));
        let root = build_image(false, &[ordinal("ordinal.dll", 12)], None);
        let items = [("root.exe", root), ("ordinal.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        let slot = &plan.slots()[0];
        assert_eq!(slot.requested_symbol, ImportName::Ordinal(12));
        let binding = resolved(slot);
        assert_eq!(binding.provider.index, 1);
        assert_eq!(binding.export, ExportKey::Ordinal(12));
        assert_eq!(binding.ordinal, 12);
        assert_eq!(binding.target_rva, CODE_RVA + 4);
        assert_eq!(binding.target_address, LIB_BASE + u64::from(CODE_RVA + 4));
    }

    #[test]
    fn transitive_and_shared_imports_bind_to_the_same_final_target() {
        const MIDDLE_BASE: u64 = 0x3_0000_0000;
        const SHARED_BASE: u64 = 0x4_0000_0000;
        const OTHER_BASE: u64 = 0x5_0000_0000;
        let shared = build_image(true, &[], Some(&export("shared.dll", &["Value"])));
        let middle = build_image(
            true,
            &[named("shared.dll", "Value")],
            Some(&export("middle.dll", &["Middle"])),
        );
        let other = build_image(
            true,
            &[named("shared.dll", "Value")],
            Some(&export("other.dll", &["Other"])),
        );
        let root = build_image(
            false,
            &[named("middle.dll", "Middle"), named("other.dll", "Other")],
            None,
        );
        let items = [
            ("root.exe", root),
            ("middle.dll", middle),
            ("shared.dll", shared),
            ("other.dll", other),
        ];
        let plan = analyze(&items, &[ROOT_BASE, MIDDLE_BASE, SHARED_BASE, OTHER_BASE]);

        assert_eq!(plan.slots().len(), 4);
        let providers: Vec<_> = plan
            .slots()
            .iter()
            .map(|slot| resolved(slot).provider.index)
            .collect();
        assert_eq!(providers, [1, 3, 2, 2]);
        let shared_target = SHARED_BASE + u64::from(CODE_RVA);
        assert_eq!(resolved(&plan.slots()[2]).target_address, shared_target);
        assert_eq!(resolved(&plan.slots()[3]).target_address, shared_target);
        assert_eq!(
            resolved(&plan.slots()[2]).slot_address,
            MIDDLE_BASE + u64::from(items[1].1.import_slots[0])
        );
        assert_eq!(
            resolved(&plan.slots()[3]).slot_address,
            OTHER_BASE + u64::from(items[3].1.import_slots[0])
        );
    }

    #[test]
    fn forwarded_import_uses_the_final_provider_base_and_export_rva() {
        const MIDDLE_BASE: u64 = 0x3_0000_0000;
        const NEXT_BASE: u64 = 0x4_0000_0000;
        const FINAL_BASE: u64 = 0x5_0000_0000;
        let final_library = build_image(true, &[], Some(&export("final.dll", &["Impl"])));
        let next_spec = ExportSpec {
            base: 1,
            module_name: "next.dll".to_owned(),
            slots: vec![slot(Target::Forward("final.Impl".to_owned()), &["Impl"])],
        };
        let next = build_image(true, &[], Some(&next_spec));
        let middle_spec = ExportSpec {
            base: 1,
            module_name: "middle.dll".to_owned(),
            slots: vec![slot(
                Target::Forward("next.Impl".to_owned()),
                &["Forwarded"],
            )],
        };
        let middle = build_image(true, &[], Some(&middle_spec));
        let root = build_image(false, &[named("middle.dll", "Forwarded")], None);
        let items = [
            ("root.exe", root),
            ("middle.dll", middle),
            ("next.dll", next),
            ("final.dll", final_library),
        ];
        let plan = analyze(&items, &[ROOT_BASE, MIDDLE_BASE, NEXT_BASE, FINAL_BASE]);

        let slot = &plan.slots()[0];
        assert_eq!(slot.forwarder_chain.len(), 2);
        assert_eq!(slot.forwarder_chain[0].target_module, "next.dll");
        assert_eq!(slot.forwarder_chain[1].target_module, "final.dll");
        assert_eq!(
            slot.forwarder_chain[1].target_export,
            ExportKey::Name("Impl".to_owned())
        );
        let binding = resolved(slot);
        assert_eq!(binding.provider.index, 3);
        assert_eq!(binding.provider.file_name, "final.dll");
        assert_eq!(binding.export, ExportKey::Name("Impl".to_owned()));
        assert_eq!(binding.target_rva, CODE_RVA);
        assert_eq!(binding.provider_base, FINAL_BASE);
        assert_eq!(binding.target_address, FINAL_BASE + u64::from(CODE_RVA));
    }

    #[test]
    fn circular_module_dependencies_still_have_static_binding_addresses() {
        const A_BASE: u64 = 0x2_0000_0000;
        const B_BASE: u64 = 0x3_0000_0000;
        let a = build_image(true, &[named("b.dll", "B")], Some(&export("a.dll", &["A"])));
        let b = build_image(true, &[named("a.dll", "A")], Some(&export("b.dll", &["B"])));
        let items = [("a.dll", a), ("b.dll", b)];
        let plan = analyze(&items, &[A_BASE, B_BASE]);

        assert_eq!(plan.slots().len(), 2);
        assert_eq!(resolved(&plan.slots()[0]).provider.index, 1);
        assert_eq!(resolved(&plan.slots()[1]).provider.index, 0);
        assert_eq!(
            resolved(&plan.slots()[0]).target_address,
            B_BASE + u64::from(CODE_RVA)
        );
        assert_eq!(
            resolved(&plan.slots()[1]).target_address,
            A_BASE + u64::from(CODE_RVA)
        );
    }

    #[test]
    fn circular_forwarders_remain_structured_unresolved_reasons() {
        let ca_spec = ExportSpec {
            base: 1,
            module_name: "ca.dll".to_owned(),
            slots: vec![slot(Target::Forward("cb.X".to_owned()), &["A"])],
        };
        let cb_spec = ExportSpec {
            base: 1,
            module_name: "cb.dll".to_owned(),
            slots: vec![slot(Target::Forward("ca.A".to_owned()), &["X"])],
        };
        let ca = build_image(true, &[], Some(&ca_spec));
        let cb = build_image(true, &[], Some(&cb_spec));
        let root = build_image(false, &[named("ca.dll", "A")], None);
        let items = [("root.exe", root), ("ca.dll", ca), ("cb.dll", cb)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE, LIB_BASE + 0x1000]);

        match import_reason(&plan.slots()[0]) {
            UnresolvedImportReason::ForwarderCycle { step } => {
                assert_eq!(step.provider.index, 2);
                assert_eq!(step.target_module, "ca.dll");
                assert_eq!(step.target_export, ExportKey::Name("A".to_owned()));
            }
            reason => panic!("forwarder cycle was not preserved: {reason:?}"),
        }
    }

    #[test]
    fn missing_modules_and_exports_are_explicit_slot_outcomes() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["Present"])));
        let root = build_image(
            false,
            &[named("absent", "X"), named("lib.dll", "Missing")],
            None,
        );
        let items = [("root.exe", root), ("lib.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        match import_reason(&plan.slots()[0]) {
            UnresolvedImportReason::MissingModule { module } => {
                assert_eq!(module, "absent.dll")
            }
            reason => panic!("missing module was not preserved: {reason:?}"),
        }
        match import_reason(&plan.slots()[1]) {
            UnresolvedImportReason::MissingExport { provider, export } => {
                assert_eq!(provider.index, 1);
                assert_eq!(*export, ExportKey::Name("Missing".to_owned()));
            }
            reason => panic!("missing export was not preserved: {reason:?}"),
        }
    }

    #[test]
    fn empty_export_slots_are_absent_rather_than_image_base_code() {
        let spec = ExportSpec {
            base: 5,
            module_name: "gap.dll".to_owned(),
            slots: vec![slot(Target::Empty, &["Gap"])],
        };
        let library = build_image(true, &[], Some(&spec));
        let root = build_image(false, &[named("gap.dll", "Gap")], None);
        let items = [("root.exe", root), ("gap.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        match import_reason(&plan.slots()[0]) {
            UnresolvedImportReason::EmptyExportSlot {
                provider,
                export,
                ordinal,
            } => {
                assert_eq!(provider.index, 1);
                assert_eq!(*export, ExportKey::Name("Gap".to_owned()));
                assert_eq!(*ordinal, 5);
            }
            reason => panic!("empty export slot was not preserved: {reason:?}"),
        }
    }

    #[test]
    fn malformed_import_tables_keep_prior_slots_and_plan_inspection_errors() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let root = build_image(
            false,
            &[named("lib.dll", "X"), ImportSpec::BrokenDescriptor],
            None,
        );
        let items = [("root.exe", root), ("lib.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        assert!(!plan.inspection_is_complete());
        assert_eq!(plan.slots().len(), 1);
        assert_eq!(resolved(&plan.slots()[0]).provider.index, 1);
        assert_eq!(
            plan.inspection_errors(),
            [ModuleInspectionError::Import {
                module: xenora_loader::module_graph::ModuleReference {
                    index: 0,
                    file_name: "root.exe".to_owned(),
                },
                error: ImportTableError::IncompleteImportDescriptor {
                    rva: IMPORT_RVA + 20,
                },
            }]
        );
    }

    #[test]
    fn malformed_export_tables_are_not_used_as_binding_providers() {
        const UNBACKED_NAME_RVA: u32 = 0x7000;
        let mut library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        put32_rva(&mut library.bytes, NPT_RVA, UNBACKED_NAME_RVA);
        let root = build_image(false, &[named("lib.dll", "X")], None);
        let items = [("root.exe", root), ("lib.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        assert!(!plan.inspection_is_complete());
        assert_eq!(
            plan.inspection_errors(),
            [ModuleInspectionError::Export {
                module: xenora_loader::module_graph::ModuleReference {
                    index: 1,
                    file_name: "lib.dll".to_owned(),
                },
                error: ExportTableError::MalformedExportName {
                    rva: UNBACKED_NAME_RVA,
                },
            }]
        );
        match import_reason(&plan.slots()[0]) {
            UnresolvedImportReason::ProviderExportsMalformed { provider } => {
                assert_eq!(provider.index, 1)
            }
            reason => panic!("malformed provider table was not preserved: {reason:?}"),
        }
    }

    #[test]
    fn malformed_forwarders_keep_the_graphs_precise_diagnostic() {
        let spec = ExportSpec {
            base: 1,
            module_name: "badfw.dll".to_owned(),
            slots: vec![slot(Target::Forward("NoSeparator".to_owned()), &["Bad"])],
        };
        let provider = build_image(true, &[], Some(&spec));
        let root = build_image(false, &[named("badfw.dll", "Bad")], None);
        let items = [("root.exe", root), ("badfw.dll", provider)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        match import_reason(&plan.slots()[0]) {
            UnresolvedImportReason::MalformedForwarder {
                provider,
                export,
                target,
                error,
            } => {
                assert_eq!(provider.index, 1);
                assert_eq!(*export, ExportKey::Name("Bad".to_owned()));
                assert_eq!(target, "NoSeparator");
                assert_eq!(*error, ForwarderParseError::MissingSeparator);
            }
            reason => panic!("malformed forwarder was not preserved: {reason:?}"),
        }
    }

    fn plan_with_second_slot_at_offset(offset: u32) -> (ModuleBindingPlan, u32) {
        let spec = ExportSpec {
            base: 1,
            module_name: "lib.dll".to_owned(),
            slots: vec![slot(Target::Export, &["A"]), slot(Target::Export, &["B"])],
        };
        let library = build_image(true, &[], Some(&spec));
        let mut root = build_image(false, &[named("lib.dll", "A"), named("lib.dll", "B")], None);
        let first_slot = root.import_slots[0];
        let second_descriptor = data_offset(IMPORT_RVA) + 20;
        put32(&mut root.bytes, second_descriptor + 16, first_slot + offset);
        let items = [("root.exe", root), ("lib.dll", library)];
        (analyze(&items, &[ROOT_BASE, LIB_BASE]), first_slot)
    }

    #[test]
    fn duplicate_iat_slots_are_not_given_an_order_dependent_winner() {
        let (plan, first_slot) = plan_with_second_slot_at_offset(0);

        assert!(plan.inspection_is_complete());
        assert_eq!(plan.slots().len(), 2);
        for slot in plan.slots() {
            assert_eq!(slot.slot_rva, first_slot);
            assert!(matches!(
                assert_overlap(slot, 2),
                ImportOutcome::Resolved { .. }
            ));
        }
    }

    #[test]
    fn partially_overlapping_iat_writes_are_not_given_an_order_dependent_winner() {
        let (plan, first_slot) = plan_with_second_slot_at_offset(4);

        assert!(plan.inspection_is_complete());
        assert_eq!(plan.slots().len(), 2);
        assert_eq!(plan.slots()[0].slot_rva, first_slot);
        assert_eq!(plan.slots()[1].slot_rva, first_slot + 4);
        for slot in plan.slots() {
            assert!(matches!(
                assert_overlap(slot, 2),
                ImportOutcome::Resolved { .. }
            ));
        }
    }

    #[test]
    fn transitively_connected_overlaps_are_one_write_cluster() {
        let spec = ExportSpec {
            base: 1,
            module_name: "lib.dll".to_owned(),
            slots: vec![
                slot(Target::Export, &["A"]),
                slot(Target::Export, &["B"]),
                slot(Target::Export, &["C"]),
            ],
        };
        let library = build_image(true, &[], Some(&spec));
        let mut root = build_image(
            false,
            &[
                named("lib.dll", "A"),
                named("lib.dll", "B"),
                named("lib.dll", "C"),
            ],
            None,
        );
        let first_slot = root.import_slots[0];
        // The first and third writes do not overlap directly, but both overlap
        // the middle write: [x, x+8), [x+4, x+12) and [x+10, x+18).
        put32(
            &mut root.bytes,
            data_offset(IMPORT_RVA) + 20 + 16,
            first_slot + 4,
        );
        put32(
            &mut root.bytes,
            data_offset(IMPORT_RVA) + 40 + 16,
            first_slot + 10,
        );
        let items = [("root.exe", root), ("lib.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        assert_eq!(plan.slots().len(), 3);
        for slot in plan.slots() {
            assert!(matches!(
                assert_overlap(slot, 3),
                ImportOutcome::Resolved { .. }
            ));
        }
    }

    #[test]
    fn overlapping_slots_preserve_missing_module_resolution() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["A"])));
        let mut root = build_image(
            false,
            &[named("missing.dll", "X"), named("lib.dll", "A")],
            None,
        );
        let first_slot = root.import_slots[0];
        put32(
            &mut root.bytes,
            data_offset(IMPORT_RVA) + 20 + 16,
            first_slot,
        );
        let items = [("root.exe", root), ("lib.dll", library)];
        let plan = analyze(&items, &[ROOT_BASE, LIB_BASE]);

        assert!(matches!(
            assert_overlap(&plan.slots()[0], 2),
            ImportOutcome::Unresolved(UnresolvedImportReason::MissingModule { module })
                if module == "missing.dll"
        ));
        assert!(matches!(
            assert_overlap(&plan.slots()[1], 2),
            ImportOutcome::Resolved { .. }
        ));
    }

    #[test]
    fn exactly_adjacent_iat_slots_do_not_overlap() {
        let (plan, first_slot) = plan_with_second_slot_at_offset(IAT_SLOT_BYTES as u32);

        assert_eq!(plan.slots()[0].slot_rva, first_slot);
        assert_eq!(plan.slots()[1].slot_rva, first_slot + 8);
        assert!(matches!(
            &plan.slots()[0].outcome,
            BindingOutcome::Resolved(_)
        ));
        assert!(matches!(
            &plan.slots()[1].outcome,
            BindingOutcome::Resolved(_)
        ));
    }

    #[test]
    fn target_address_arithmetic_is_checked_without_rejecting_the_exact_boundary() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let root = build_image(false, &[named("lib.dll", "X")], None);
        let items = [("root.exe", root), ("lib.dll", library)];
        let package = package(&items);
        let graph = ModuleGraph::build(&package).unwrap();

        let overflow = ModuleBindingPlan::build(&graph, &[ROOT_BASE, u64::MAX]).unwrap();
        match unresolved(&overflow.slots()[0]) {
            UnresolvedBindingReason::TargetAddressOverflow {
                provider,
                export,
                ordinal,
                target_rva,
                provider_base,
            } => {
                assert_eq!(provider.index, 1);
                assert_eq!(*export, ExportKey::Name("X".to_owned()));
                assert_eq!(*ordinal, 1);
                assert_eq!(*target_rva, CODE_RVA);
                assert_eq!(*provider_base, u64::MAX);
            }
            reason => panic!("target overflow was not diagnosed: {reason:?}"),
        }

        let boundary_base = u64::MAX - u64::from(CODE_RVA);
        let boundary = ModuleBindingPlan::build(&graph, &[ROOT_BASE, boundary_base]).unwrap();
        assert_eq!(resolved(&boundary.slots()[0]).target_address, u64::MAX);
    }

    #[test]
    fn consumer_slot_address_and_slot_width_are_checked() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let mut root = build_image(false, &[named("lib.dll", "X")], None);
        // Ordinary import parsing does not need to read the IAT bytes. Put the
        // first slot at RVA one so base arithmetic can reach the exact end of
        // the u64 range without overflowing the starting-address addition.
        put32(&mut root.bytes, data_offset(IMPORT_RVA) + 16, 1);
        let items = [("root.exe", root), ("lib.dll", library)];
        let package = package(&items);
        let graph = ModuleGraph::build(&package).unwrap();

        let valid_base = u64::MAX - IAT_SLOT_BYTES;
        let valid = ModuleBindingPlan::build(&graph, &[valid_base, LIB_BASE]).unwrap();
        assert_eq!(
            resolved(&valid.slots()[0]).slot_address,
            u64::MAX - (IAT_SLOT_BYTES - 1)
        );

        let invalid_base = u64::MAX - (IAT_SLOT_BYTES - 1);
        let invalid = ModuleBindingPlan::build(&graph, &[invalid_base, LIB_BASE]).unwrap();
        match unresolved(&invalid.slots()[0]) {
            UnresolvedBindingReason::SlotAddressOverflow {
                consumer,
                slot_rva,
                consumer_base,
            } => {
                assert_eq!(consumer.index, 0);
                assert_eq!(*slot_rva, 1);
                assert_eq!(*consumer_base, invalid_base);
            }
            reason => panic!("slot width overflow was not diagnosed: {reason:?}"),
        }
    }

    #[test]
    fn base_addresses_are_indexed_by_package_order_and_must_be_complete() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let root = build_image(false, &[named("lib.dll", "X")], None);
        let items = [("root.exe", root), ("lib.dll", library)];
        let package = package(&items);
        let graph = ModuleGraph::build(&package).unwrap();

        assert_eq!(
            ModuleBindingPlan::build(&graph, &[ROOT_BASE]),
            Err(ModuleBindingError::BaseAddressCount {
                expected: 2,
                actual: 1,
            })
        );
    }
}
