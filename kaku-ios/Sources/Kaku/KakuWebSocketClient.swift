import Foundation

// MARK: - Wire types (mirrors Rust ScreenUpdate)

struct ScreenUpdate: Decodable {
    let pane_id: Int
    let cursor_x: Int
    let cursor_y: Int
    let cols: Int
    let viewport_rows: Int
    let scrollback_top: Int?
    let physical_top: Int?
    let scrollback_rows: Int?
    let view_top: Int?
    let lines: [ScreenLine]
}

struct ScreenLine: Decodable {
    let row: Int
    let text: String
}

// MARK: - Delegate

@MainActor
protocol KakuWebSocketDelegate: AnyObject {
    func didReceiveScreen(_ update: ScreenUpdate)
    func didDisconnect(error: Error?)
}

// MARK: - Client

@MainActor
final class KakuWebSocketClient: NSObject {
    private var task: URLSessionWebSocketTask?
    private var endpointURL: URL?
    /// Incremented each time a new connection is started; callbacks from old
    /// tasks compare against this value and no-op if they're stale.
    private var generation = 0

    weak var delegate: KakuWebSocketDelegate?

    var isConnected: Bool { task?.state == .running }

    /// LAN mode: connect directly to the desktop's local HTTP WebSocket server.
    func connect(host: String, port: Int, paneId: Int, token: String) {
        let urlString = "ws://\(host):\(port)/ws/\(paneId)"
        guard let url = URL(string: urlString) else {
            KakuLog.add("[WS] invalid URL")
            return
        }
        if endpointURL == url, task?.state == .running {
            return
        }
        cancelCurrent()
        endpointURL = url
        KakuLog.add("[WS] connecting → \(urlString)")
        var request = URLRequest(url: url)
        request.setValue(token, forHTTPHeaderField: "x-kaku-token")
        task = URLSession.shared.webSocketTask(with: request)
        task?.resume()
        receive(generation: generation)
    }

    /// Relay mode: connect to the public relay server as a client endpoint.
    /// The relay server has a valid TLS cert, so the system session is used.
    func connectRelay(server: String, token: String) {
        let urlString = "wss://\(server)/c/\(token)"
        guard let url = URL(string: urlString) else {
            KakuLog.add("[WS] relay invalid URL")
            return
        }
        if endpointURL == url, task?.state == .running {
            return
        }
        cancelCurrent()
        endpointURL = url
        KakuLog.add("[WS] relay connecting → \(urlString)")
        task = URLSession.shared.webSocketTask(with: url)
        task?.resume()
        receive(generation: generation)
    }

    func disconnect() {
        cancelCurrent()
    }

    private func cancelCurrent() {
        generation += 1
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
        endpointURL = nil
    }

    /// Send keyboard input. `paneId` is required in relay mode so the desktop
    /// can route the input to the correct PTY.
    func send(text: String, paneId: Int? = nil) {
        let payload: String
        if let paneId {
            payload = "{\"type\":\"input\",\"pane_id\":\(paneId),\"text\":\(jsonString(text))}"
        } else {
            payload = "{\"type\":\"input\",\"text\":\(jsonString(text))}"
        }
        task?.send(.string(payload)) { _ in }
    }

    /// Scroll viewport by N lines. Positive means older lines (up), negative
    /// means newer lines (down).
    func sendScroll(delta: Int, paneId: Int? = nil) {
        guard delta != 0 else { return }
        let payload: String
        if let paneId {
            payload = "{\"type\":\"scroll\",\"pane_id\":\(paneId),\"delta\":\(delta)}"
        } else {
            payload = "{\"type\":\"scroll\",\"delta\":\(delta)}"
        }
        task?.send(.string(payload)) { _ in }
    }

    // MARK: - Private

    private func receive(generation gen: Int) {
        task?.receive { [weak self] result in
            guard let self else { return }
            Task { @MainActor [weak self] in
                guard let self, self.generation == gen else { return }
                switch result {
                case .success(let msg):
                    if case .string(let text) = msg,
                       let data = text.data(using: .utf8),
                       let update = try? JSONDecoder().decode(ScreenUpdate.self, from: data) {
                        self.delegate?.didReceiveScreen(update)
                    }
                    self.receive(generation: gen)
                case .failure(let err):
                    KakuLog.add("[WS] error: \(err)")
                    let ns = err as NSError
                    if ns.domain == NSPOSIXErrorDomain, ns.code == 57 {
                        let hint = NSError(
                            domain: "KakuRelay",
                            code: 57,
                            userInfo: [
                                NSLocalizedDescriptionKey:
                                    "Socket is not connected. Verify desktop Kaku has remote tunnel enabled and uses token 862d2e7f53ae7c68."
                            ]
                        )
                        self.delegate?.didDisconnect(error: hint)
                    } else {
                        self.delegate?.didDisconnect(error: err)
                    }
                }
            }
        }
    }

    private func jsonString(_ s: String) -> String {
        guard let data = try? JSONEncoder().encode(s),
              let str = String(data: data, encoding: .utf8) else { return "\"\(s)\"" }
        return str
    }
}

// MARK: - REST helpers

struct KakuAPI {
    let host: String
    let port: Int
    let token: String

    private var base: URL { URL(string: "http://\(host):\(port)")! }

    func fetchConfig() async throws -> KakuConfigResponse {
        let url = base.appendingPathComponent("api/config")
        KakuLog.add("[HTTP] GET \(url)")
        var req = URLRequest(url: url)
        req.setValue(token, forHTTPHeaderField: "x-kaku-token")
        do {
            let (data, resp) = try await URLSession.shared.data(for: req)
            KakuLog.add("[HTTP] \((resp as? HTTPURLResponse)?.statusCode ?? 0) (\(data.count) bytes)")
            return try JSONDecoder().decode(KakuConfigResponse.self, from: data)
        } catch {
            KakuLog.add("[HTTP] failed: \(error)")
            throw error
        }
    }

    func fetchPanes() async throws -> [PaneInfo] {
        let url = base.appendingPathComponent("api/panes")
        KakuLog.add("[HTTP] GET \(url)")
        var req = URLRequest(url: url)
        req.setValue(token, forHTTPHeaderField: "x-kaku-token")
        do {
            let (data, resp) = try await URLSession.shared.data(for: req)
            KakuLog.add("[HTTP] \((resp as? HTTPURLResponse)?.statusCode ?? 0) (\(data.count) bytes)")
            return try JSONDecoder().decode([PaneInfo].self, from: data)
        } catch {
            KakuLog.add("[HTTP] failed: \(error)")
            throw error
        }
    }
}

struct PaneInfo: Decodable, Identifiable {
    let id: Int
    let title: String
    let cwd: String?
}

// MARK: - Simple in-app logger

private let _logDateFormatter: DateFormatter = {
    let f = DateFormatter()
    f.dateFormat = "HH:mm:ss.SSS"
    return f
}()

final class KakuLog: ObservableObject {
    static let shared = KakuLog()
    private init() {}

    @MainActor @Published private(set) var entries: [String] = []

    nonisolated static func add(_ message: String) {
        let line = "\(_logDateFormatter.string(from: Date())) \(message)"
        print("[Kaku] \(line)")
        Task { @MainActor in
            shared.entries.append(line)
            if shared.entries.count > 200 { shared.entries.removeFirst() }
        }
    }

    nonisolated static func clear() {
        Task { @MainActor in shared.entries.removeAll() }
    }
}
