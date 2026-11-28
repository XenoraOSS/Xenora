// SPDX-License-Identifier: GPL-3.0-or-later

import SwiftUI
import UniformTypeIdentifiers

@MainActor
struct AuthoredPESessionView: View {
    private struct Companion: Identifiable {
        let id = UUID()
        let url: URL
        var pin = ""
    }

    private enum PickerTarget {
        case executable, companions
    }

    private static let maxCompanions = 255
    let inspectionURL: URL?
    let inspectedExecutable: URL?
    @ObservedObject var model: AuthoredPESessionModel
    @State private var selectedExecutable: URL?
    @State private var executablePin = ""
    @State private var companions: [Companion] = []
    @State private var backend: AuthoredPESessionBackend = .reference
    @State private var pickerTarget: PickerTarget = .executable
    @State private var picking = false
    @State private var selectionError: String?
    @State private var input = ""

    private var executable: URL? { selectedExecutable ?? inspectedExecutable }

    private var active: Bool {
        switch model.phase {
        case .starting, .ready, .rendering, .stopping: true
        case .idle, .finished, .failed, .cancelled: false
        }
    }

    private var ready: Bool {
        if case .ready = model.phase { return true }
        return false
    }

    private var acceptingInput: Bool {
        ready && model.frames < AuthoredPESessionTranscript.maxFrames
    }

    private var configuration: AuthoredPESessionConfiguration? {
        guard let executable, let pin = Self.digest(executablePin) else { return nil }
        let images = companions.compactMap { companion in
            Self.digest(companion.pin).map { AuthoredPESessionImage(url: companion.url, sha256: $0) }
        }
        guard images.count == companions.count else { return nil }
        return AuthoredPESessionConfiguration(
            executable: AuthoredPESessionImage(url: executable, sha256: pin),
            companions: images,
            backend: backend
        )
    }

    private var inputValue: UInt32? {
        guard !input.isEmpty, input.utf8.allSatisfy({ (48...57).contains($0) }) else { return nil }
        return UInt32(input)
    }

    var body: some View {
        Section("Authored PE Session") {
            setup
                .disabled(active)
            if let selectionError {
                Text(selectionError).foregroundStyle(.red)
            }

            HStack {
                Button("Start session") { start() }
                    .disabled(active || configuration == nil)
                Button("Finish session") { model.finish() }
                    .disabled(!ready)
                Button("Cancel session", role: .cancel) { model.cancel() }
                    .disabled(!active)
            }

            HStack {
                TextField("Frame input", text: $input, prompt: Text("0–4,294,967,295"))
                    .onSubmit { send() }
                    .disabled(!acceptingInput)
                Button("Send input") { send() }
                    .disabled(!acceptingInput || inputValue == nil)
            }

            Text(model.statusText)
                .font(.callout)
                .textSelection(.enabled)
            if let frame = model.frame {
                if let image = frame.image {
                    Image(decorative: image, scale: 1)
                        .resizable()
                        .interpolation(.none)
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, minHeight: 96, maxHeight: 240)
                        .accessibilityLabel("Verified guest frame \(frame.index), \(frame.width) by \(frame.height) pixels")
                }
                LabeledContent("Verified frame", value: "\(frame.index) · \(frame.width) × \(frame.height)")
                LabeledContent("Frame SHA-256") {
                    Text(frame.sha256)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                }
            }
        }
        .fileImporter(isPresented: $picking, allowedContentTypes: [.item],
                      allowsMultipleSelection: pickerTarget == .companions, onCompletion: picked)
        .onChange(of: inspectionURL) {
            guard selectedExecutable == nil else { return }
            if active { model.cancel() }
            resetSelection()
        }
    }

    private var setup: some View {
        Group {
            LabeledContent("Root executable") {
                HStack {
                    Text(executable?.lastPathComponent ?? "None selected")
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Button("Choose executable…") { choose(.executable) }
                }
            }
            if executable != nil {
                TextField("Executable SHA-256", text: $executablePin, prompt: Text("64 hexadecimal digits"))
                    .font(.body.monospaced())
            }
            ForEach($companions) { $companion in
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text(companion.url.lastPathComponent)
                        Spacer()
                        Button("Remove") { companions.removeAll { $0.id == companion.id } }
                            .accessibilityLabel("Remove \(companion.url.lastPathComponent)")
                    }
                    TextField("Companion SHA-256", text: $companion.pin, prompt: Text("64 hexadecimal digits"))
                        .font(.body.monospaced())
                        .accessibilityLabel("SHA-256 for \(companion.url.lastPathComponent)")
                }
            }
            Button("Add companion DLLs…") { choose(.companions) }
                .disabled(executable == nil || companions.count >= Self.maxCompanions)
            Picker("Graphics backend", selection: $backend) {
                Text("Reference").tag(AuthoredPESessionBackend.reference)
                Text("Metal").tag(AuthoredPESessionBackend.metal)
            }
        }
    }

    private func choose(_ target: PickerTarget) {
        pickerTarget = target
        selectionError = nil
        picking = true
    }

    private func picked(_ result: Result<[URL], Error>) {
        guard !active else { return }
        switch result {
        case .success(let urls):
            switch pickerTarget {
            case .executable:
                guard let url = urls.first else { return }
                let sameExecutable = url == executable
                selectedExecutable = url
                if !sameExecutable { resetSelection() }
            case .companions:
                addCompanions(urls)
            }
        case .failure(let error):
            if (error as? CocoaError)?.code != .userCancelled {
                selectionError = "Could not select the file: \(error.localizedDescription)"
            }
        }
    }

    private func addCompanions(_ urls: [URL]) {
        guard let executable else { return }
        guard urls.count <= Self.maxCompanions - companions.count else {
            selectionError = "A session accepts up to \(Self.maxCompanions) companion DLLs."
            return
        }
        let reserved = "xenora-session-v1.dll"
        guard !([executable] + urls).contains(where: { $0.lastPathComponent.lowercased() == reserved }) else {
            selectionError = "\(reserved) is reserved for the session protocol."
            return
        }
        var names = Set(([executable] + companions.map(\.url)).map { $0.lastPathComponent.lowercased() })
        for url in urls {
            let name = url.lastPathComponent
            guard names.insert(name.lowercased()).inserted else {
                selectionError = "The package already contains an image named \(name)."
                return
            }
        }
        companions.append(contentsOf: urls.map { Companion(url: $0) })
    }

    private func resetSelection() {
        executablePin = ""
        companions = []
        selectionError = nil
        input = ""
    }

    private func start() {
        guard !active, let configuration else { return }
        input = ""
        model.start(configuration)
    }

    private func send() {
        guard acceptingInput, let inputValue else { return }
        model.send(inputValue)
        input = ""
    }

    private static func digest(_ value: String) -> String? {
        let pin = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard pin.utf8.count == 64, pin.utf8.allSatisfy({
            (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
        }) else { return nil }
        return pin.lowercased()
    }
}
