// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional explicit-module IAT binding tests (#894).
//!
//! The PE images are original byte-built synthetic fixtures. No host library
//! or proprietary module takes part in these tests.

mod module_iat {
    use xenora_loader::module_binding::{
        ModuleBindingPlan, ResolvedBinding, UnresolvedBindingReason,
    };
    use xenora_loader::module_graph::{
        ExportKey, ImportOutcome, ModuleGraph, PackageModule, UnresolvedImportReason,
    };
    use xenora_loader::module_iat::{
        IatSlotOutcome, IatSlotReport, ModuleIatError, ModuleIatReport, bind_module_iat,
    };
    use xenora_loader::module_layout::{
        ModuleImageDescriptor, ModuleLayoutArena, ModuleLayoutPlan,
    };
    use xenora_loader::module_memory::{ExplicitModuleImage, ModuleReservation};
    use xenora_loader::{GuestMemory, MemoryError, MemoryPermissions, ParsedPe, map_module_batch};

    const BASE: u64 = 0x1_4000_0000;
    const BASE_STRIDE: u64 = 0x1_0000;
    const SCRATCH: u64 = 0x1_8000_0000;
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
        Named { module: String, symbol: String },
        Ordinal { module: String, ordinal: u16 },
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
            let (module, lookup_value) = match import {
                ImportSpec::Named { module, symbol } => {
                    let hint = allocate(&mut cursor, symbol.len() + 3, IMPORT_END);
                    put(pe, data_offset(hint) + 2, symbol.as_bytes());
                    (module, u64::from(hint))
                }
                ImportSpec::Ordinal { module, ordinal } => {
                    (module, (1u64 << 63) | u64::from(*ordinal))
                }
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

    struct PackageImage {
        file_name: String,
        image: TestImage,
        pe: ParsedPe,
    }

    struct Package {
        images: Vec<PackageImage>,
    }

    impl Package {
        fn new(items: Vec<(&str, TestImage)>) -> Self {
            let images = items
                .into_iter()
                .enumerate()
                .map(|(index, (file_name, mut image))| {
                    // Give every module a distinct aligned preferred base. The
                    // layout planner then chooses deterministic preferred
                    // placements without making these tests depend on fallback
                    // search behavior.
                    let base = BASE + BASE_STRIDE * index as u64;
                    put(&mut image.bytes, OPTIONAL + 24, &base.to_le_bytes());
                    let pe = ParsedPe::parse(&image.bytes).expect("synthetic image parses");
                    PackageImage {
                        file_name: file_name.to_owned(),
                        image,
                        pe,
                    }
                })
                .collect();
            Self { images }
        }

        fn package_modules(&self) -> Vec<PackageModule<'_>> {
            self.images
                .iter()
                .map(|image| PackageModule {
                    file_name: image.file_name.as_str(),
                    bytes: image.image.bytes.as_slice(),
                })
                .collect()
        }

        fn explicit_images(&self) -> Vec<ExplicitModuleImage<'_>> {
            self.images
                .iter()
                .map(|image| ExplicitModuleImage {
                    pe: &image.pe,
                    bytes: image.image.bytes.as_slice(),
                })
                .collect()
        }

        fn graph(&self) -> ModuleGraph {
            let modules = self.package_modules();
            ModuleGraph::build(&modules).expect("synthetic package graph builds")
        }
    }

    fn arena() -> ModuleLayoutArena {
        ModuleLayoutArena {
            base: 0x1_0000_0000,
            size: 0x1_0000_0000,
        }
    }

    fn map_images(
        images: &[ExplicitModuleImage<'_>],
        memory: &mut GuestMemory,
    ) -> Vec<ModuleReservation> {
        let descriptors: Vec<_> = images
            .iter()
            .map(|image| ModuleImageDescriptor::from_parsed_pe(image.pe))
            .collect();
        let layout = ModuleLayoutPlan::build(arena(), &[], &descriptors)
            .expect("synthetic layout plan builds");
        assert!(layout.all_placed());
        map_module_batch(images, &layout, memory).expect("synthetic package maps")
    }

    fn bases(reservations: &[ModuleReservation]) -> Vec<u64> {
        reservations
            .iter()
            .map(|reservation| reservation.base)
            .collect()
    }

    fn correct_plan(graph: &ModuleGraph, reservations: &[ModuleReservation]) -> ModuleBindingPlan {
        ModuleBindingPlan::build(graph, &bases(reservations)).expect("base count matches")
    }

    fn bind_correct_plan(
        images: &[ExplicitModuleImage<'_>],
        reservations: &[ModuleReservation],
        graph: &ModuleGraph,
        memory: &mut GuestMemory,
    ) -> Result<ModuleIatReport, ModuleIatError> {
        let plan = correct_plan(graph, reservations);
        bind_module_iat(images, reservations, graph, &plan, memory)
    }

    fn written(slot: &IatSlotReport) -> &ResolvedBinding {
        match &slot.outcome {
            IatSlotOutcome::Written(binding) => binding,
            outcome => panic!("slot is not written: {outcome:?}"),
        }
    }

    fn assert_planned_memory_bytes(memory: &GuestMemory, binding: &ResolvedBinding) {
        let mut actual = [0; 8];
        memory
            .read_bytes(binding.slot_address, &mut actual)
            .unwrap();
        assert_eq!(actual, binding.target_address.to_le_bytes());
    }

    fn read_slot_bytes(memory: &GuestMemory, address: u64) -> [u8; 8] {
        let mut bytes = [0; 8];
        memory.read_bytes(address, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn an_empty_package_is_a_complete_empty_transaction() {
        let graph = ModuleGraph::build(&[]).unwrap();
        let plan = ModuleBindingPlan::build(&graph, &[]).unwrap();
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(SCRATCH, 1, MemoryPermissions::READ_WRITE, &[0x77])
            .unwrap();

        let report = bind_module_iat(&[], &[], &graph, &plan, &mut memory).unwrap();

        assert!(report.inspection_is_complete());
        assert!(report.images.is_empty());
        assert_eq!(report.slot_count(), 0);
        assert_eq!(memory.read_u8(SCRATCH), Ok(0x77));
    }

    #[test]
    fn named_and_ordinal_slots_write_planned_bytes_and_report_per_image() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["Start"])));
        let ordinal_spec = ExportSpec {
            base: 11,
            module_name: "ordinal.dll".to_owned(),
            slots: vec![slot(Target::Export, &[]), slot(Target::Export, &[])],
        };
        let ordinal_library = build_image(true, &[], Some(&ordinal_spec));
        let root = build_image(
            false,
            &[named("lib.dll", "Start"), ordinal("ordinal.dll", 12)],
            None,
        );
        let package = Package::new(vec![
            ("root.exe", root),
            ("lib.dll", library),
            ("ordinal.dll", ordinal_library),
        ]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);

        let report = bind_correct_plan(&images, &reservations, &graph, &mut memory).unwrap();

        assert!(report.inspection_is_complete());
        assert_eq!(report.images.len(), 3);
        assert_eq!(report.slot_count(), 2);
        assert_eq!(report.written_slot_count(), 2);
        assert_eq!(report.unresolved_slot_count(), 0);
        assert_eq!(report.images[0].image_index, 0);
        assert_eq!(report.images[0].slots.len(), 2);
        assert!(report.images[1].slots.is_empty());
        assert!(report.images[2].slots.is_empty());

        let named_binding = written(&report.images[0].slots[0]);
        assert_eq!(named_binding.provider.index, 1);
        assert_eq!(named_binding.export, ExportKey::Name("Start".to_owned()));
        assert_eq!(named_binding.consumer_base, reservations[0].base);
        assert_eq!(named_binding.provider_base, reservations[1].base);
        assert_eq!(
            named_binding.target_address,
            reservations[1].base + u64::from(CODE_RVA)
        );
        assert_planned_memory_bytes(&memory, named_binding);

        let ordinal_binding = written(&report.images[0].slots[1]);
        assert_eq!(ordinal_binding.provider.index, 2);
        assert_eq!(ordinal_binding.export, ExportKey::Ordinal(12));
        assert_eq!(ordinal_binding.ordinal, 12);
        assert_eq!(ordinal_binding.target_rva, CODE_RVA + 4);
        assert_eq!(
            ordinal_binding.target_address,
            reservations[2].base + u64::from(CODE_RVA + 4)
        );
        assert_planned_memory_bytes(&memory, ordinal_binding);
    }

    #[test]
    fn loader_binding_can_write_a_read_only_iat_section_without_changing_permissions() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let mut root = build_image(false, &[named("lib.dll", "X")], None);
        // `.data` remains readable initialized data but loses IMAGE_SCN_MEM_WRITE.
        let data_characteristics = OPTIONAL + 240 + 40 + 36;
        put32(&mut root.bytes, data_characteristics, 0x4000_0040);
        let package = Package::new(vec![("root.exe", root), ("lib.dll", library)]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);

        let report = bind_correct_plan(&images, &reservations, &graph, &mut memory).unwrap();

        let binding = written(&report.images[0].slots[0]);
        assert_planned_memory_bytes(&memory, binding);
        assert_eq!(
            memory.write_u8(binding.slot_address, 0),
            Err(MemoryError::PermissionViolation {
                address: binding.slot_address,
                required: "write",
            })
        );
    }

    #[test]
    fn forwarded_import_writes_the_final_providers_static_address() {
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
        let package = Package::new(vec![
            ("root.exe", root),
            ("middle.dll", middle),
            ("next.dll", next),
            ("final.dll", final_library),
        ]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);

        let report = bind_correct_plan(&images, &reservations, &graph, &mut memory).unwrap();

        assert_eq!(report.images[0].slots.len(), 1);
        let slot = &report.images[0].slots[0];
        assert_eq!(slot.forwarder_chain.len(), 2);
        assert_eq!(slot.forwarder_chain[0].target_module, "next.dll");
        assert_eq!(slot.forwarder_chain[1].target_module, "final.dll");
        let binding = written(slot);
        assert_eq!(binding.provider.index, 3);
        assert_eq!(binding.provider.file_name, "final.dll");
        assert_eq!(binding.export, ExportKey::Name("Impl".to_owned()));
        assert_eq!(binding.provider_base, reservations[3].base);
        assert_eq!(
            binding.target_address,
            reservations[3].base + u64::from(CODE_RVA)
        );
        assert_planned_memory_bytes(&memory, binding);
    }

    fn assert_overlap_outcome(slot: &IatSlotReport, entries: usize) {
        match &slot.outcome {
            IatSlotOutcome::Unresolved(UnresolvedBindingReason::OverlappingIatSlot {
                entries: actual,
                resolution,
            }) => {
                assert_eq!(*actual, entries);
                assert!(matches!(**resolution, ImportOutcome::Resolved { .. }));
            }
            outcome => panic!("slot overlap was not preserved: {outcome:?}"),
        }
    }

    #[test]
    fn overlapping_slots_remain_unwritten_diagnostics() {
        let spec = ExportSpec {
            base: 1,
            module_name: "lib.dll".to_owned(),
            slots: vec![slot(Target::Export, &["A"]), slot(Target::Export, &["B"])],
        };
        let library = build_image(true, &[], Some(&spec));
        let mut root = build_image(false, &[named("lib.dll", "A"), named("lib.dll", "B")], None);
        let first_slot = root.import_slots[0];
        let second_descriptor = data_offset(IMPORT_RVA) + 20;
        put32(&mut root.bytes, second_descriptor + 16, first_slot + 4);
        let package = Package::new(vec![("root.exe", root), ("lib.dll", library)]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);
        let first_address = reservations[0].base + u64::from(first_slot);
        let mut before = [0; 12];
        memory.read_bytes(first_address, &mut before).unwrap();

        let report = bind_correct_plan(&images, &reservations, &graph, &mut memory).unwrap();

        assert_eq!(report.slot_count(), 2);
        assert_eq!(report.written_slot_count(), 0);
        assert_eq!(report.unresolved_slot_count(), 2);
        assert_overlap_outcome(&report.images[0].slots[0], 2);
        assert_overlap_outcome(&report.images[0].slots[1], 2);
        let mut after = [0; 12];
        memory.read_bytes(first_address, &mut after).unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn missing_providers_and_exports_remain_unwritten_diagnostics() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["Present"])));
        let root = build_image(
            false,
            &[named("absent", "X"), named("lib.dll", "Missing")],
            None,
        );
        let package = Package::new(vec![("root.exe", root), ("lib.dll", library)]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);
        let addresses: Vec<_> = package.images[0]
            .image
            .import_slots
            .iter()
            .map(|slot| reservations[0].base + u64::from(*slot))
            .collect();
        let before: Vec<_> = addresses
            .iter()
            .map(|address| read_slot_bytes(&memory, *address))
            .collect();

        let report = bind_correct_plan(&images, &reservations, &graph, &mut memory).unwrap();

        assert_eq!(report.written_slot_count(), 0);
        assert_eq!(report.unresolved_slot_count(), 2);
        match &report.images[0].slots[0].outcome {
            IatSlotOutcome::Unresolved(UnresolvedBindingReason::Import(
                UnresolvedImportReason::MissingModule { module },
            )) => assert_eq!(module, "absent.dll"),
            outcome => panic!("missing provider was not preserved: {outcome:?}"),
        }
        match &report.images[0].slots[1].outcome {
            IatSlotOutcome::Unresolved(UnresolvedBindingReason::Import(
                UnresolvedImportReason::MissingExport { provider, export },
            )) => {
                assert_eq!(provider.index, 1);
                assert_eq!(*export, ExportKey::Name("Missing".to_owned()));
            }
            outcome => panic!("missing export was not preserved: {outcome:?}"),
        }
        for (address, original) in addresses.iter().zip(before) {
            assert_eq!(read_slot_bytes(&memory, *address), original);
        }
    }

    #[test]
    fn backing_validation_after_an_earlier_write_restores_the_exact_prior_bytes() {
        let spec = ExportSpec {
            base: 1,
            module_name: "lib.dll".to_owned(),
            slots: vec![slot(Target::Export, &["A"]), slot(Target::Export, &["B"])],
        };
        let library = build_image(true, &[], Some(&spec));
        let mut root = build_image(false, &[named("lib.dll", "A"), named("lib.dll", "B")], None);
        let first_slot = root.import_slots[0];
        // Ordinary import parsing does not read the IAT bytes. Point the
        // second slot into the unmapped gap between .text and .data; the
        // logical SizeOfImage reservation covers that gap, but the image
        // mapping does not commit it.
        put32(&mut root.bytes, data_offset(IMPORT_RVA) + 20 + 16, 0x1800);
        let package = Package::new(vec![("root.exe", root), ("lib.dll", library)]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);
        let first_address = reservations[0].base + u64::from(first_slot);
        let original_first = read_slot_bytes(&memory, first_address);
        let plan = correct_plan(&graph, &reservations);

        let error = bind_module_iat(&images, &reservations, &graph, &plan, &mut memory)
            .expect_err("an image hole is not IAT backing");

        assert!(matches!(
            error,
            ModuleIatError::SlotImageBackingMissing {
                slot_rva: 0x1800,
                ..
            }
        ));
        assert_eq!(read_slot_bytes(&memory, first_address), original_first);
    }

    #[test]
    fn write_failure_after_an_earlier_slot_restores_the_exact_prior_bytes() {
        let provider = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let first = build_image(false, &[named("lib.dll", "X")], None);
        let second = build_image(false, &[named("lib.dll", "X")], None);
        let package = Package::new(vec![
            ("first.exe", first),
            ("second.exe", second),
            ("lib.dll", provider),
        ]);
        let images = package.explicit_images();
        let graph = package.graph();

        // Produce valid reservations for the complete package, then bind a
        // memory image that contains only the first consumer. Reservations are
        // logical records, not ownership tokens; the write to the absent second
        // image is the memory failure under test.
        let mut authoritative = GuestMemory::new();
        let reservations = map_images(&images, &mut authoritative);
        drop(authoritative);
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(SCRATCH, 0x40, MemoryPermissions::READ_WRITE, &[0x77; 0x40])
            .unwrap();
        images[0]
            .pe
            .map_into_memory(images[0].bytes, &mut memory, reservations[0].base)
            .unwrap();
        let first_slot = package.images[0].image.import_slots[0];
        let first_address = reservations[0].base + u64::from(first_slot);
        let original_first = read_slot_bytes(&memory, first_address);
        let second_slot = package.images[1].image.import_slots[0];
        let second_address = reservations[1].base + u64::from(second_slot);
        let plan = correct_plan(&graph, &reservations);

        let error = bind_module_iat(&images, &reservations, &graph, &plan, &mut memory)
            .expect_err("the absent second consumer cannot be written");

        match error {
            ModuleIatError::WriteFailed {
                consumer,
                slot_rva,
                slot_address,
                source,
            } => {
                assert_eq!(consumer.index, 1);
                assert_eq!(slot_rva, second_slot);
                assert_eq!(slot_address, second_address);
                assert_eq!(source, MemoryError::UnmappedAddress(second_address));
            }
            error => panic!("memory write failure was not structured: {error:?}"),
        }
        assert_eq!(read_slot_bytes(&memory, first_address), original_first);
        assert_eq!(
            memory.read_u8(second_address),
            Err(MemoryError::UnmappedAddress(second_address))
        );
        let mut retained = [0; 0x40];
        memory.read_bytes(SCRATCH, &mut retained).unwrap();
        assert_eq!(retained, [0x77; 0x40]);
        memory.write_bytes(SCRATCH, &[0x78]).unwrap();
        assert_eq!(memory.read_u8(SCRATCH), Ok(0x78));
    }

    #[test]
    fn wrong_binding_bases_are_rejected_before_memory_mutation() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let root = build_image(false, &[named("lib.dll", "X")], None);
        let package = Package::new(vec![("root.exe", root), ("lib.dll", library)]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);
        let slot = package.images[0].image.import_slots[0];
        let address = reservations[0].base + u64::from(slot);
        let original = read_slot_bytes(&memory, address);
        let mut wrong_bases = bases(&reservations);
        wrong_bases[1] += 0x1000;
        let wrong_plan = ModuleBindingPlan::build(&graph, &wrong_bases).unwrap();

        let error = bind_module_iat(&images, &reservations, &graph, &wrong_plan, &mut memory)
            .expect_err("a plan built at other bases is not corresponding");

        match error {
            ModuleIatError::BindingPlanMismatch {
                slot_position,
                plan_slots,
                expected_slots,
                inspection_errors,
            } => {
                assert_eq!(slot_position, Some(0));
                assert_eq!(plan_slots, 1);
                assert_eq!(expected_slots, 1);
                assert!(!inspection_errors);
            }
            error => panic!("wrong binding bases were not structured: {error:?}"),
        }
        assert_eq!(read_slot_bytes(&memory, address), original);
    }

    #[test]
    fn graph_from_other_image_bytes_is_rejected_before_memory_mutation() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["Start"])));
        let correct_root = build_image(false, &[named("lib.dll", "Start")], None);
        let correct = Package::new(vec![("root.exe", correct_root), ("lib.dll", library)]);
        let images = correct.explicit_images();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);
        let slot = correct.images[0].image.import_slots[0];
        let address = reservations[0].base + u64::from(slot);
        let original = read_slot_bytes(&memory, address);

        let other_library = build_image(true, &[], Some(&export("lib.dll", &["Start"])));
        let other_root = build_image(false, &[named("lib.dll", "Missing")], None);
        let other = Package::new(vec![("root.exe", other_root), ("lib.dll", other_library)]);
        let other_graph = other.graph();
        let other_plan = correct_plan(&other_graph, &reservations);

        let error = bind_module_iat(
            &images,
            &reservations,
            &other_graph,
            &other_plan,
            &mut memory,
        )
        .expect_err("a graph built from other image bytes is not the package");

        assert!(matches!(
            error,
            ModuleIatError::ImageGraphMismatch {
                image_index: Some(0),
                source: None,
            }
        ));
        assert_eq!(read_slot_bytes(&memory, address), original);
    }

    #[test]
    fn reservation_shape_and_input_indexes_are_validated_before_memory_mutation() {
        let library = build_image(true, &[], Some(&export("lib.dll", &["X"])));
        let root = build_image(false, &[named("lib.dll", "X")], None);
        let package = Package::new(vec![("root.exe", root), ("lib.dll", library)]);
        let images = package.explicit_images();
        let graph = package.graph();
        let mut memory = GuestMemory::new();
        let reservations = map_images(&images, &mut memory);
        let slot = package.images[0].image.import_slots[0];
        let address = reservations[0].base + u64::from(slot);
        let original = read_slot_bytes(&memory, address);
        let plan = correct_plan(&graph, &reservations);

        assert_eq!(
            bind_module_iat(&images, &reservations[..1], &graph, &plan, &mut memory),
            Err(ModuleIatError::InputCountMismatch {
                module_images: 2,
                reservations: 1,
                graph_modules: 2,
            })
        );

        let mut wrong_index = reservations.clone();
        wrong_index[0].image_index = 1;
        assert_eq!(
            bind_module_iat(&images, &wrong_index, &graph, &plan, &mut memory),
            Err(ModuleIatError::ReservationIndexMismatch {
                position: 0,
                image_index: 1,
            })
        );

        let mut wrong_span = reservations.clone();
        wrong_span[0].end += 8;
        assert_eq!(
            bind_module_iat(&images, &wrong_span, &graph, &plan, &mut memory),
            Err(ModuleIatError::InvalidReservationSpan {
                image_index: 0,
                base: reservations[0].base,
                end: reservations[0].end + 8,
                image_size: DATA_RVA + DATA_LEN_U32,
            })
        );
        assert_eq!(read_slot_bytes(&memory, address), original);
    }
}
