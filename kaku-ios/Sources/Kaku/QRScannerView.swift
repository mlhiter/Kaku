import SwiftUI
import VisionKit

// MARK: - Result

struct KakuQRResult {
    let host: String
    let port: Int
    let token: String
    /// Non-nil when this is a relay QR: value is the relay hostname (e.g. "kaku-relay.fly.dev").
    let relayServer: String?

    var isRelay: Bool { relayServer != nil }

    /// Parse both URL schemes:
    ///   LAN:   kakuremote://192.168.x.x:9988?token=xxx
    ///   Relay: kakuremote://relay?server=kaku-relay.fly.dev&token=xxx
    static func parse(_ string: String) -> KakuQRResult? {
        guard let url = URL(string: string), url.scheme == "kakuremote" else { return nil }
        let comps = URLComponents(url: url, resolvingAgainstBaseURL: false)
        guard let token = comps?.queryItems?.first(where: { $0.name == "token" })?.value,
              !token.isEmpty else { return nil }

        if url.host == "relay" {
            // Relay mode: kakuremote://relay?server=xxx&token=yyy
            guard let server = comps?.queryItems?.first(where: { $0.name == "server" })?.value,
                  !server.isEmpty else { return nil }
            return KakuQRResult(host: server, port: 443, token: token, relayServer: server)
        } else {
            // LAN mode: kakuremote://host:port?token=xxx
            guard let host = url.host, !host.isEmpty else { return nil }
            return KakuQRResult(host: host, port: url.port ?? 9988, token: token, relayServer: nil)
        }
    }
}

// MARK: - SwiftUI wrapper

struct QRScannerView: UIViewControllerRepresentable {
    var onFound: (KakuQRResult) -> Void
    var onCancel: () -> Void

    func makeUIViewController(context: Context) -> UIViewController {
        guard DataScannerViewController.isSupported,
              DataScannerViewController.isAvailable else {
            return UnsupportedScannerViewController(onCancel: onCancel)
        }

        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            recognizesMultipleItems: false,
            isHighFrameRateTrackingEnabled: false,
            isPinchToZoomEnabled: true,
            isGuidanceEnabled: true,
            isHighlightingEnabled: true
        )
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ vc: UIViewController, context: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(onFound: onFound)
    }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        var onFound: (KakuQRResult) -> Void
        private var processed = false

        init(onFound: @escaping (KakuQRResult) -> Void) {
            self.onFound = onFound
        }

        func dataScanner(
            _ scanner: DataScannerViewController,
            didAdd addedItems: [RecognizedItem],
            allItems: [RecognizedItem]
        ) {
            guard !processed else { return }
            for item in addedItems {
                if case .barcode(let barcode) = item,
                   let raw = barcode.payloadStringValue,
                   let result = KakuQRResult.parse(raw) {
                    processed = true
                    scanner.stopScanning()
                    DispatchQueue.main.async { self.onFound(result) }
                    return
                }
            }
        }
    }
}

// MARK: - Fallback for simulator / unsupported devices

private final class UnsupportedScannerViewController: UIViewController {
    let onCancel: () -> Void
    init(onCancel: @escaping () -> Void) { self.onCancel = onCancel; super.init(nibName: nil, bundle: nil) }
    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = UIColor.systemBackground

        let label = UILabel()
        label.text = "Camera scanning not available on this device.\nEnter connection details manually."
        label.numberOfLines = 0
        label.textAlignment = .center
        label.textColor = .secondaryLabel
        label.translatesAutoresizingMaskIntoConstraints = false

        let button = UIButton(type: .system)
        button.setTitle("Dismiss", for: .normal)
        button.addTarget(self, action: #selector(dismiss_), for: .touchUpInside)
        button.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(label)
        view.addSubview(button)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            label.centerYAnchor.constraint(equalTo: view.centerYAnchor, constant: -30),
            label.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 32),
            label.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -32),
            button.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            button.topAnchor.constraint(equalTo: label.bottomAnchor, constant: 20),
        ])
    }

    @objc private func dismiss_() { onCancel() }
}
