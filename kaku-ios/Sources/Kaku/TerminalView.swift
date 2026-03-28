import SwiftUI
import SwiftTerm

// MARK: - Font size preference

private let fontSizeKey = "kaku_font_size_override"

func loadFontSizeOverride() -> CGFloat? {
    let v = UserDefaults.standard.double(forKey: fontSizeKey)
    return v > 0 ? CGFloat(v) : nil
}

func saveFontSizeOverride(_ size: CGFloat) {
    UserDefaults.standard.set(Double(size), forKey: fontSizeKey)
}

// MARK: - SwiftUI wrapper

struct KakuTerminalView: UIViewRepresentable {
    let update: ScreenUpdate?
    let theme: KakuTheme
    let onInput: (String) -> Void
    let onScroll: (Int) -> Void
    let fitToWidth: Bool
    let cols: Int
    let containerWidth: CGFloat

    init(
        update: ScreenUpdate?,
        theme: KakuTheme,
        onInput: @escaping (String) -> Void,
        onScroll: @escaping (Int) -> Void,
        fitToWidth: Bool = false,
        cols: Int = 80,
        containerWidth: CGFloat = 0
    ) {
        self.update = update
        self.theme = theme
        self.onInput = onInput
        self.onScroll = onScroll
        self.fitToWidth = fitToWidth
        self.cols = cols
        self.containerWidth = containerWidth
    }

    func makeUIView(context: Context) -> TerminalView {
        let tv = TerminalView(frame: .zero)
        tv.terminalDelegate = context.coordinator
        context.coordinator.terminalView = tv
        context.coordinator.attach(to: tv)
        applyTheme(tv, theme: theme, coordinator: context.coordinator)
        attachGestures(to: tv, coordinator: context.coordinator)
        return tv
    }

    func updateUIView(_ tv: TerminalView, context: Context) {
        context.coordinator.onInput = onInput
        context.coordinator.onScroll = onScroll
        context.coordinator.cols = cols
        context.coordinator.fitToWidth = fitToWidth
        context.coordinator.containerWidth = containerWidth
        if let update {
            context.coordinator.applyUpdate(update, to: tv)
        }
        applyTheme(tv, theme: theme, coordinator: context.coordinator)
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onInput: onInput, onScroll: onScroll)
    }

    // MARK: - Theme

    private func applyTheme(_ tv: TerminalView, theme: KakuTheme, coordinator: Coordinator) {
        tv.nativeBackgroundColor = UIColor(theme.background)
        tv.nativeForegroundColor = UIColor(theme.foreground)
        // User's persisted font size overrides theme; otherwise use theme size
        let baseSize = loadFontSizeOverride() ?? theme.fontSize
        let size: CGFloat
        if coordinator.fitToWidth, coordinator.cols > 0, coordinator.containerWidth > 0 {
            // Calculate font size to fit all cols within container width
            // Use more accurate char width ratio and ensure minimum readability
            let charWidthRatio: CGFloat = 0.6
            let padding: CGFloat = 32  // 16pt padding each side
            let availableWidth = coordinator.containerWidth - padding

            // Calculate required scale
            let neededWidth = CGFloat(coordinator.cols) * (baseSize * charWidthRatio)

            if neededWidth > availableWidth {
                let scale = availableWidth / neededWidth
                // Ensure minimum 10pt for readability, don't go below that
                size = max(8, baseSize * scale)
                print("[TerminalView] fitToWidth: cols=\(coordinator.cols), base=\(baseSize), scale=\(scale), final=\(size)")
            } else {
                size = baseSize
            }
        } else {
            size = baseSize
        }
        coordinator.currentFontSize = size
        let font = theme.terminalFont(fitSize: size)
        coordinator.currentFontName = theme.fontFamily
        if tv.font.fontName != font.fontName || tv.font.pointSize != font.pointSize {
            tv.font = font
        }
    }

    // MARK: - Gestures

    private func attachGestures(to tv: TerminalView, coordinator: Coordinator) {
        let pinch = UIPinchGestureRecognizer(
            target: coordinator, action: #selector(Coordinator.handlePinch(_:)))
        tv.addGestureRecognizer(pinch)

        let twoFingerSwipe = UIPanGestureRecognizer(
            target: coordinator, action: #selector(Coordinator.handleTwoFingerPan(_:)))
        twoFingerSwipe.minimumNumberOfTouches = 2
        twoFingerSwipe.maximumNumberOfTouches = 2
        tv.addGestureRecognizer(twoFingerSwipe)
    }

    // MARK: - Coordinator

    final class Coordinator: NSObject, TerminalViewDelegate, TerminalInputAccessoryDelegate {
        weak var terminalView: TerminalView?
        var onInput: (String) -> Void
        var onScroll: (Int) -> Void
        var currentFontSize: CGFloat = 16
        var currentFontName: String = "JetBrainsMonoNerdFont-Regular"
        var fitToWidth: Bool = false
        var cols: Int = 80
        var containerWidth: CGFloat = 0
        private var lastRenderedLines: [Int: String] = [:]
        private var pinchStartSize: CGFloat = 16
        private var panLineRemainder: CGFloat = 0

        // inputAccessoryView attached once
        private lazy var accessory: TerminalInputAccessoryView = {
            let v = TerminalInputAccessoryView(
                frame: CGRect(x: 0, y: 0, width: 0, height: 56))
            v.delegate = self
            return v
        }()

        init(onInput: @escaping (String) -> Void, onScroll: @escaping (Int) -> Void) {
            self.onInput = onInput
            self.onScroll = onScroll
            super.init()
        }

        // Suppress system shortcut bar; accessory is attached via TerminalView.inputAccessoryView
        func attach(to tv: TerminalView) {
            tv.inputAssistantItem.leadingBarButtonGroups = []
            tv.inputAssistantItem.trailingBarButtonGroups = []
            tv.inputAccessoryView = accessory
        }

        // MARK: Screen update

        func applyUpdate(_ update: ScreenUpdate, to tv: TerminalView) {
            let viewTop = update.view_top ?? (update.cursor_y - update.viewport_rows + 1)
            let visibleRows = Set(update.lines.map(\.row))
            for line in update.lines {
                let prev = lastRenderedLines[line.row]
                guard prev != line.text else { continue }
                lastRenderedLines[line.row] = line.text
                let localRow = line.row - viewTop
                if localRow >= 0 && localRow < update.viewport_rows {
                    let seq = "\u{1B}[\(localRow + 1);1H\(line.text)\u{1B}[K"
                    tv.feed(byteArray: ArraySlice(seq.utf8))
                }
            }
            lastRenderedLines = lastRenderedLines.filter { visibleRows.contains($0.key) }
            let cursorRow = update.cursor_y - viewTop
            if cursorRow >= 0 && cursorRow < update.viewport_rows {
                let cursorSeq = "\u{1B}[\(cursorRow + 1);\(update.cursor_x + 1)H"
                tv.feed(byteArray: ArraySlice(cursorSeq.utf8))
            }
        }

        // MARK: Gestures

        @objc func handlePinch(_ g: UIPinchGestureRecognizer) {
            guard let tv = terminalView else { return }
            switch g.state {
            case .began:
                pinchStartSize = currentFontSize
            case .changed:
                let newSize = (pinchStartSize * g.scale).clamped(to: 10...32)
                guard abs(newSize - currentFontSize) > 0.5 else { return }
                currentFontSize = newSize
                // Use font with emoji fallback
                let font = fontWithFallback(name: currentFontName, size: newSize)
                tv.font = font
            case .ended, .cancelled:
                saveFontSizeOverride(currentFontSize)
            default:
                break
            }
        }

        @objc func handleTwoFingerPan(_ g: UIPanGestureRecognizer) {
            guard let tv = terminalView else { return }
            let dy = g.translation(in: tv).y
            g.setTranslation(.zero, in: tv)
            // Scroll ~1 line per 20pt drag
            panLineRemainder += -dy / 20
            let lines = Int(panLineRemainder.rounded(.towardZero))
            guard lines != 0 else {
                if g.state == .ended || g.state == .cancelled {
                    panLineRemainder = 0
                }
                return
            }
            panLineRemainder -= CGFloat(lines)
            onScroll(lines)
            if g.state == .ended || g.state == .cancelled {
                panLineRemainder = 0
            }
        }

        // MARK: TerminalInputAccessoryDelegate

        func accessory(didSend sequence: String) {
            onInput(sequence)
        }

        // MARK: TerminalViewDelegate

        func send(source: TerminalView, data: ArraySlice<UInt8>) {
            let text = String(bytes: data, encoding: .utf8) ?? ""
            onInput(text)
        }

        func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {}
        func setTerminalTitle(source: TerminalView, title: String) {}
        func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
        func scrolled(source: TerminalView, position: Double) {}
        func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {}
        func bell(source: TerminalView) {}
        func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
        func clipboardCopy(source: TerminalView, content: Data) {}
        func iTermContent(source: TerminalView, content: ArraySlice<UInt8>) {}
    }
}

// MARK: - Helpers

private extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        max(range.lowerBound, min(range.upperBound, self))
    }
}

// MARK: - Font with Emoji Fallback

/// Creates a font with system emoji fallback using cascade list
func fontWithFallback(name: String, size: CGFloat) -> UIFont {
    // Try custom font first - with fallback variants
    var customFont: UIFont? = UIFont(name: name, size: size)
    if customFont == nil {
        let variants = [
            name,
            name.replacingOccurrences(of: "-", with: ""),
            name + "-Regular",
            "JetBrainsMono-Regular",
        ]
        for variant in variants {
            if let font = UIFont(name: variant, size: size) {
                customFont = font
                print("[TerminalView] Found font with variant: \(variant)")
                break
            }
        }
    }

    guard let customFont = customFont else {
        print("[TerminalView] Font '\(name)' not found, using system monospace")
        return UIFont.monospacedSystemFont(ofSize: size, weight: .regular)
    }

    print("[TerminalView] Using font: \(customFont.fontName)")

    let customDescriptor = customFont.fontDescriptor
    let systemDescriptor = UIFont.systemFont(ofSize: size).fontDescriptor

    // Cascade list: custom font first, then system font for emoji fallback
    let cascadeList: [[UIFontDescriptor.AttributeName: Any]] = [
        [.name: customDescriptor.postscriptName ?? name],
        [.name: systemDescriptor.postscriptName ?? ".AppleSystemUIFont"]
    ]

    let descriptorWithFallback = customDescriptor.addingAttributes([
        .cascadeList: cascadeList
    ])

    return UIFont(descriptor: descriptorWithFallback, size: size)
}
