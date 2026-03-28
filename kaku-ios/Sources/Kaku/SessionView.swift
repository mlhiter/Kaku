import SwiftUI
import UIKit

// MARK: - Session view model

@MainActor
final class SessionViewModel: ObservableObject, KakuWebSocketDelegate {
    @Published var screen: ScreenUpdate?
    @Published var theme: KakuTheme = .default
    @Published var connectionState: ConnectionState = .connecting
    /// Relay mode: pane_id → latest ScreenUpdate. Populated as updates arrive.
    @Published var relayPanes: [Int: ScreenUpdate] = [:]
    @Published var activePaneId: Int?

    enum ConnectionState {
        case connecting, connected, disconnected(String?)
    }

    private let client = KakuWebSocketClient()

    // LAN mode fields
    let api: KakuAPI?
    let pane: PaneInfo?

    // Relay mode fields
    let relayServer: String?
    let relayToken: String?

    var isRelayMode: Bool { relayServer != nil }

    var navigationTitle: String {
        if isRelayMode { return "Kaku Remote" }
        return pane?.title ?? "Terminal"
    }

    init(api: KakuAPI, pane: PaneInfo) {
        self.api = api
        self.pane = pane
        self.relayServer = nil
        self.relayToken = nil
        client.delegate = self
    }

    init(relayServer: String, token: String) {
        self.api = nil
        self.pane = nil
        self.relayServer = relayServer
        self.relayToken = token
        client.delegate = self
    }

    func start() {
        // Debug: list available fonts
        DispatchQueue.main.async {
            print("[SessionView] Checking font availability...")
            if isFontAvailable("JetBrainsMonoNerdFont-Regular") {
                print("[SessionView] ✓ JetBrainsMonoNerdFont-Regular is available")
            } else {
                print("[SessionView] ✗ JetBrainsMonoNerdFont-Regular NOT found")
                // List some common monospace fonts
                let monoFonts = ["Menlo", "Courier", "Courier New", "SF Mono"]
                for font in monoFonts {
                    if isFontAvailable(font) {
                        print("[SessionView]   Available: \(font)")
                    }
                }
            }
        }

        if isRelayMode {
            guard let server = relayServer, let token = relayToken else { return }
            client.connectRelay(server: server, token: token)
            // Stay in .connecting until first ScreenUpdate confirms the tunnel is live.
        } else {
            Task {
                if let api, let config = try? await api.fetchConfig() {
                    theme = KakuTheme.from(config)
                }
                if let api, let pane {
                    client.connect(host: api.host, port: api.port, paneId: pane.id, token: api.token)
                }
                connectionState = .connected
            }
        }
    }

    func stop() { client.disconnect() }

    func sendInput(_ text: String) {
        client.send(text: text, paneId: isRelayMode ? activePaneId : nil)
    }

    func sendScroll(delta: Int) {
        client.sendScroll(delta: delta, paneId: isRelayMode ? activePaneId : nil)
    }

    func switchPane(_ paneId: Int) {
        activePaneId = paneId
        screen = relayPanes[paneId]
    }

    // KakuWebSocketDelegate
    func didReceiveScreen(_ update: ScreenUpdate) {
        if isRelayMode {
            relayPanes[update.pane_id] = update
            if activePaneId == nil {
                // Auto-select first pane and transition out of "connecting" state.
                activePaneId = update.pane_id
                connectionState = .connected
            }
            if update.pane_id == activePaneId { screen = update }
        } else {
            screen = update
        }
    }

    func didDisconnect(error: Error?) {
        connectionState = .disconnected(error?.localizedDescription)
    }
}

// MARK: - Session view

struct SessionView: View {
    @StateObject private var vm: SessionViewModel
    @State private var fontSizeBump: CGFloat = 0
    @State private var showPanePicker = false
    @State private var showCommandBar = false
    @State private var commandText = ""
    @State private var fitToWidth = true

    /// LAN mode
    init(api: KakuAPI, pane: PaneInfo) {
        _vm = StateObject(wrappedValue: SessionViewModel(api: api, pane: pane))
    }

    /// Relay mode
    init(relayServer: String, token: String) {
        _vm = StateObject(wrappedValue: SessionViewModel(relayServer: relayServer, token: token))
    }

    private var effectiveTheme: KakuTheme {
        guard fontSizeBump != 0 else { return vm.theme }
        var t = vm.theme
        t.fontSize += fontSizeBump
        return t
    }

    var body: some View {
        ZStack(alignment: .bottom) {
            terminalLayer
            bottomArea
        }
        .overlay(alignment: .bottomTrailing) {
            if showsJumpToBottom {
                Button {
                    jumpToBottom()
                } label: {
                    Label("回到底部", systemImage: "arrow.down")
                        .font(.system(.footnote, design: .rounded, weight: .semibold))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                }
                .buttonStyle(.borderedProminent)
                .padding(.trailing, 16)
                .padding(.bottom, 80)
            }
        }
        .ignoresSafeArea(edges: .bottom)
        .navigationTitle(vm.navigationTitle)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            // Relay mode: pane switcher button when multiple panes are available.
            if vm.isRelayMode && vm.relayPanes.count > 1 {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showPanePicker = true
                    } label: {
                        Label("切换 pane", systemImage: "square.split.2x1")
                    }
                }
            }
        }
        .confirmationDialog(
            "选择 pane",
            isPresented: $showPanePicker,
            titleVisibility: .visible
        ) {
            ForEach(vm.relayPanes.keys.sorted(), id: \.self) { id in
                Button("pane \(id)\(id == vm.activePaneId ? " ✓" : "")") {
                    vm.switchPane(id)
                }
            }
        }
        .preferredColorScheme(vm.theme.colorScheme)
        .task { vm.start() }
        .onDisappear { vm.stop() }
    }

    // MARK: - Terminal layer

    @ViewBuilder
    private var terminalLayer: some View {
        switch vm.connectionState {
        case .connecting:
            vm.theme.background
                .ignoresSafeArea()
                .overlay {
                    ProgressView("Connecting…").tint(vm.theme.foreground)
                }

        case .connected:
            GeometryReader { geo in
                KakuTerminalView(
                    update: vm.screen,
                    theme: effectiveTheme,
                    onInput: { vm.sendInput($0) },
                    onScroll: { vm.sendScroll(delta: $0) },
                    fitToWidth: fitToWidth,
                    cols: vm.screen?.cols ?? 80,
                    containerWidth: geo.size.width
                )
            }

        case .disconnected(let msg):
            vm.theme.background.ignoresSafeArea()
                .overlay {
                    VStack(spacing: 20) {
                        Image(systemName: "wifi.slash")
                            .font(.system(size: 52))
                            .foregroundStyle(.orange)
                        Text("Disconnected")
                            .font(.title2.bold())
                            .foregroundStyle(vm.theme.foreground)
                        if let msg {
                            Text(msg)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .multilineTextAlignment(.center)
                        }
                        Button("Reconnect") { vm.start() }
                            .buttonStyle(.borderedProminent)
                    }
                    .padding()
                }
        }
    }

    // MARK: - Bottom area

    private var bottomArea: some View {
        VStack(spacing: 8) {
            if showCommandBar {
                commandBar
            }
            bottomBar
        }
        .padding(.bottom, 8)
    }

    private var commandBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "terminal")
                .foregroundStyle(.secondary)
            TextField("输入命令后回车发送", text: $commandText)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .submitLabel(.send)
                .onSubmit { sendCommandLine() }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 12)
    }

    // MARK: - Bottom toolbar

    private var bottomBar: some View {
        HStack(spacing: 16) {
            // Font size controls
            Button {
                adjustFontSize(-1)
            } label: {
                Image(systemName: "textformat.size.smaller")
            }
            Button {
                adjustFontSize(+1)
            } label: {
                Image(systemName: "textformat.size.larger")
            }

            // Fit to width toggle
            Button {
                fitToWidth.toggle()
            } label: {
                Image(systemName: fitToWidth ? "arrow.up.left.and.arrow.down.right" : "arrow.down.right.and.arrow.up.left")
                    .foregroundStyle(fitToWidth ? .blue : .primary)
            }

            Spacer()

            Button {
                pasteClipboard()
            } label: {
                Image(systemName: "doc.on.clipboard")
            }

            Button {
                showCommandBar.toggle()
            } label: {
                Image(systemName: showCommandBar ? "keyboard.chevron.compact.down" : "keyboard")
            }

            Spacer()

            // Disconnect
            Button(role: .destructive) {
                vm.stop()
            } label: {
                Image(systemName: "xmark.circle")
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial)
    }

    // MARK: - Helpers

    private var currentViewTop: Int? {
        guard let screen = vm.screen else { return nil }
        if let viewTop = screen.view_top {
            return viewTop
        }
        return screen.cursor_y - screen.viewport_rows + 1
    }

    private var currentPhysicalTop: Int? {
        vm.screen?.physical_top
    }

    private var showsJumpToBottom: Bool {
        guard let viewTop = currentViewTop, let physicalTop = currentPhysicalTop else { return false }
        return viewTop < physicalTop
    }

    private func jumpToBottom() {
        guard let viewTop = currentViewTop, let physicalTop = currentPhysicalTop else { return }
        let delta = viewTop - physicalTop
        guard delta != 0 else { return }
        vm.sendScroll(delta: delta)
    }

    private func pasteClipboard() {
        guard let text = UIPasteboard.general.string, !text.isEmpty else { return }
        vm.sendInput(text)
    }

    private func sendCommandLine() {
        let text = commandText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        vm.sendInput(text + "\n")
        commandText = ""
    }

    private func adjustFontSize(_ delta: CGFloat) {
        let current = loadFontSizeOverride() ?? (vm.theme.fontSize + fontSizeBump)
        let newSize = (current + delta).clamped(to: 10...32)
        fontSizeBump = newSize - vm.theme.fontSize
        saveFontSizeOverride(newSize)
        fitToWidth = false  // Manual override disables auto-fit
    }
}

private extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        max(range.lowerBound, min(range.upperBound, self))
    }
}
