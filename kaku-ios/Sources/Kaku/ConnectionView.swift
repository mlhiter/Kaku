import SwiftUI

// MARK: - Saved connection

struct KakuConnection: Codable, Equatable {
    var host: String = ""
    var port: Int = 9988
    var token: String = ""
    /// Non-nil when this connection uses the relay server instead of direct LAN.
    var relayServer: String?

    var isRelay: Bool { relayServer != nil }
}

// MARK: - Relay navigation destination

struct RelayDestination: Hashable {
    let server: String
    let token: String
}

// MARK: - Connection view

struct ConnectionView: View {
    @AppStorage("kaku_connection") private var stored = Data()
    @State private var conn = KakuConnection()
    @State private var path = NavigationPath()
    @State private var pasteURL = ""
    @State private var connectionError: String?
    @State private var isConnecting = false
    @State private var showScanner = false
    @State private var showAdvanced = false
    @State private var showLog = false
    @ObservedObject private var log = KakuLog.shared
    private let forcedRelayURL = "kakuremote://relay?server=kaku-relay.fly.dev&token=862d2e7f53ae7c68"

    var body: some View {
        NavigationStack(path: $path) {
            VStack(spacing: 0) {
                header
                Divider()
                ScrollView {
                    VStack(spacing: 20) {
                        pasteSection
                        orDivider
                        scanSection
                        advancedSection
                        if let err = connectionError {
                            Label(err, systemImage: "exclamationmark.triangle")
                                .foregroundStyle(.red)
                                .font(.footnote)
                                .padding(.horizontal)
                        }
                        logSection
                    }
                    .padding(.vertical, 24)
                }
            }
            .navigationBarHidden(true)
            .navigationDestination(for: KakuAPI.self) { api in
                PaneListView(api: api) { pane in
                    path.append(SessionDestination(api: api, pane: pane))
                }
            }
            .navigationDestination(for: SessionDestination.self) { dest in
                SessionView(api: dest.api, pane: dest.pane)
            }
            .navigationDestination(for: RelayDestination.self) { dest in
                SessionView(relayServer: dest.server, token: dest.token)
            }
            .sheet(isPresented: $showScanner) {
                QRScannerSheet { result in
                    showScanner = false
                    conn.host        = result.host
                    conn.port        = result.port
                    conn.token       = result.token
                    conn.relayServer = result.relayServer
                    connect()
                } onCancel: {
                    showScanner = false
                }
            }
            .onAppear {
                loadSaved()
                applyForcedRelayConfiguration()
            }
        }
    }

    // MARK: - Sections

    private var header: some View {
        HStack {
            Image(systemName: "terminal.fill")
                .font(.title2)
                .foregroundStyle(.tint)
            Text("Kaku")
                .font(.title2.bold())
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }

    private var pasteSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("粘贴连接地址")
                .font(.subheadline.weight(.medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 20)

            HStack(spacing: 10) {
                TextField("kakuremote://192.168.x.x:9988?token=…", text: $pasteURL)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    .padding(12)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                    .onChange(of: pasteURL) { _ in tryAutoParse() }

                Button {
                    pasteURL = UIPasteboard.general.string ?? ""
                    tryAutoParse()
                } label: {
                    Image(systemName: "doc.on.clipboard")
                        .padding(12)
                        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                }
            }
            .padding(.horizontal, 20)

            Button {
                connect()
            } label: {
                connectLabel
            }
            .disabled(isConnecting)
            .padding(.horizontal, 20)
        }
    }

    private var orDivider: some View {
        HStack {
            Rectangle().frame(height: 1).foregroundStyle(Color(.separator))
            Text("或").font(.footnote).foregroundStyle(.secondary).padding(.horizontal, 8)
            Rectangle().frame(height: 1).foregroundStyle(Color(.separator))
        }
        .padding(.horizontal, 20)
    }

    private var scanSection: some View {
        Button {
            showScanner = true
        } label: {
            Label("扫描二维码", systemImage: "qrcode.viewfinder")
                .frame(maxWidth: .infinity)
                .padding(14)
                .background(Color.accentColor, in: RoundedRectangle(cornerRadius: 12))
                .foregroundStyle(.white)
                .font(.body.weight(.medium))
        }
        .padding(.horizontal, 20)
    }

    private var advancedSection: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { showAdvanced.toggle() }
            } label: {
                HStack {
                    Text("手动填写")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Image(systemName: showAdvanced ? "chevron.up" : "chevron.down")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 10)
            }

            if showAdvanced {
                VStack(spacing: 12) {
                    TextField("Host", text: $conn.host)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .padding(12)
                        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                        .padding(.horizontal, 20)

                    HStack {
                        Text("Port").foregroundStyle(.secondary).font(.subheadline)
                        Spacer()
                        TextField("9988", value: $conn.port, format: .number)
                            .keyboardType(.numberPad)
                            .multilineTextAlignment(.trailing)
                            .frame(width: 80)
                    }
                    .padding(12)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                    .padding(.horizontal, 20)

                    SecureField("Token", text: $conn.token)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .padding(12)
                        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                        .padding(.horizontal, 20)

                    Button { connect() } label: { connectLabel }
                        .disabled(conn.host.isEmpty || conn.token.isEmpty || isConnecting)
                        .padding(.horizontal, 20)
                }
                .padding(.bottom, 8)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .background(Color(.secondarySystemBackground).opacity(0.5), in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 20)
    }

    @ViewBuilder
    private var connectLabel: some View {
        if isConnecting {
            HStack { ProgressView(); Text("连接中…").padding(.leading, 8) }
                .frame(maxWidth: .infinity)
                .padding(14)
                .background(Color(.systemGray4), in: RoundedRectangle(cornerRadius: 12))
        } else {
            Text("连接")
                .frame(maxWidth: .infinity)
                .padding(14)
                .background(Color(.systemGray4), in: RoundedRectangle(cornerRadius: 12))
                .font(.body.weight(.medium))
        }
    }

    // MARK: - Helpers

    // MARK: - Log panel

    private var logSection: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { showLog.toggle() }
            } label: {
                HStack {
                    Image(systemName: "ladybug")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("调试日志 (\(log.entries.count))")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Spacer()
                    if !log.entries.isEmpty {
                        Button("清除") { KakuLog.clear() }
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Image(systemName: showLog ? "chevron.up" : "chevron.down")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 10)
            }

            if showLog {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 2) {
                            ForEach(Array(log.entries.enumerated()), id: \.offset) { i, line in
                                Text(line)
                                    .font(.system(size: 11, design: .monospaced))
                                    .foregroundStyle(.primary)
                                    .textSelection(.enabled)
                                    .id(i)
                            }
                        }
                        .padding(10)
                    }
                    .frame(height: 200)
                    .background(Color(.systemBackground))
                    .onChange(of: log.entries.count) { _ in
                        if let last = log.entries.indices.last {
                            proxy.scrollTo(last, anchor: .bottom)
                        }
                    }
                }

                Button {
                    UIPasteboard.general.string = log.entries.joined(separator: "\n")
                } label: {
                    Label("复制全部日志", systemImage: "doc.on.doc")
                        .font(.caption)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 8)
                }
                .foregroundStyle(.secondary)
            }
        }
        .background(Color(.secondarySystemBackground).opacity(0.5), in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 20)
        .onAppear {
            // Log ATS info on first open to help diagnose
            if let ats = Bundle.main.infoDictionary?["NSAppTransportSecurity"] {
                KakuLog.add("[ATS] Info.plist entry: \(ats)")
            } else {
                KakuLog.add("[ATS] ⚠️ NSAppTransportSecurity not found in Info.plist")
            }
        }
    }

    private func tryAutoParse() {
        guard let result = KakuQRResult.parse(pasteURL) else { return }
        conn.host        = result.host
        conn.port        = result.port
        conn.token       = result.token
        conn.relayServer = result.relayServer
        connectionError  = nil
    }

    private func connect() {
        connectionError = nil
        isConnecting = true
        applyForcedRelayConfiguration()
        save()

        // Relay mode: no REST API available — navigate directly to relay session.
        if conn.isRelay, let server = conn.relayServer {
            isConnecting = false
            path.append(RelayDestination(server: server, token: conn.token))
            return
        }

        // LAN mode: verify connection with a panes fetch first.
        let api = KakuAPI(host: conn.host, port: conn.port, token: conn.token)
        Task {
            do {
                _ = try await api.fetchPanes()
                await MainActor.run { isConnecting = false; path.append(api) }
            } catch {
                await MainActor.run { isConnecting = false; connectionError = error.localizedDescription }
            }
        }
    }

    private func save() {
        stored = (try? JSONEncoder().encode(conn)) ?? Data()
    }

    private func loadSaved() {
        if let c = try? JSONDecoder().decode(KakuConnection.self, from: stored) {
            conn = c
        }
    }

    private func applyForcedRelayConfiguration() {
        guard let forced = KakuQRResult.parse(forcedRelayURL) else { return }
        pasteURL = forcedRelayURL
        conn.host = forced.host
        conn.port = forced.port
        conn.token = forced.token
        conn.relayServer = forced.relayServer
    }
}

// MARK: - QR scanner sheet

private struct QRScannerSheet: View {
    var onFound: (KakuQRResult) -> Void
    var onCancel: () -> Void
    var body: some View {
        NavigationStack {
            QRScannerView(onFound: onFound, onCancel: onCancel)
                .ignoresSafeArea()
                .navigationTitle("扫描二维码")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) { Button("取消", action: onCancel) }
                }
        }
    }
}

// MARK: - Navigation helpers

extension KakuAPI: Hashable {
    public static func == (lhs: KakuAPI, rhs: KakuAPI) -> Bool { lhs.host == rhs.host && lhs.port == rhs.port }
    public func hash(into hasher: inout Hasher) { hasher.combine(host); hasher.combine(port) }
}

struct SessionDestination: Hashable {
    let api: KakuAPI
    let pane: PaneInfo
    static func == (l: Self, r: Self) -> Bool { l.pane.id == r.pane.id }
    func hash(into h: inout Hasher) { h.combine(pane.id) }
}
