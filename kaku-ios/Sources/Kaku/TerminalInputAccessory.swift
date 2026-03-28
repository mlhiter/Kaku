import UIKit

private enum KeyKind {
    case esc
    case tab
    case ctrl
    case alt
    case sequence(String)
    case text(String)
    case ctrlShortcut(UInt8)
}

private struct KeyDef {
    let label: String
    let kind: KeyKind

    static func seq(_ label: String, _ value: String) -> Self {
        Self(label: label, kind: .sequence(value))
    }

    static func text(_ label: String, _ value: String) -> Self {
        Self(label: label, kind: .text(value))
    }

    static func shortcut(_ label: String, _ control: UInt8) -> Self {
        Self(label: label, kind: .ctrlShortcut(control))
    }
}

private let keys: [KeyDef] = [
    KeyDef(label: "Esc", kind: .esc),
    KeyDef(label: "Tab", kind: .tab),
    KeyDef(label: "Ctrl", kind: .ctrl),
    KeyDef(label: "Alt", kind: .alt),
    .seq("←", "\u{1B}[D"),
    .seq("↓", "\u{1B}[B"),
    .seq("↑", "\u{1B}[A"),
    .seq("→", "\u{1B}[C"),
    .text("~", "~"),
    .text("/", "/"),
    .text("|", "|"),
    .text("-", "-"),
    .shortcut("Ctrl+R", 0x12),
    .shortcut("Ctrl+L", 0x0C),
    .shortcut("Ctrl+Z", 0x1A),
]

protocol TerminalInputAccessoryDelegate: AnyObject {
    func accessory(didSend sequence: String)
}

final class TerminalInputAccessoryView: UIView {
    weak var delegate: TerminalInputAccessoryDelegate?

    private var ctrlArmed = false {
        didSet { updateModifierButtonAppearance() }
    }
    private var altArmed = false {
        didSet { updateModifierButtonAppearance() }
    }

    private var ctrlButton: UIButton?
    private var altButton: UIButton?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        buildUI()
    }

    required init?(coder: NSCoder) { fatalError() }

    override var intrinsicContentSize: CGSize {
        CGSize(width: UIView.noIntrinsicMetric, height: 56)
    }

    private func buildUI() {
        let scroll = UIScrollView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.alwaysBounceHorizontal = true
        scroll.showsHorizontalScrollIndicator = false

        let stack = UIStackView()
        stack.axis = .horizontal
        stack.alignment = .center
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false

        addSubview(scroll)
        scroll.addSubview(stack)

        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),

            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 6),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -6),
            stack.heightAnchor.constraint(greaterThanOrEqualTo: scroll.frameLayoutGuide.heightAnchor, constant: -12),
        ])

        for key in keys {
            let button = makeButton(for: key)
            if key.label == "Ctrl" {
                ctrlButton = button
            } else if key.label == "Alt" {
                altButton = button
            }
            stack.addArrangedSubview(button)
        }
    }

    private func makeButton(for key: KeyDef) -> UIButton {
        var config = UIButton.Configuration.filled()
        config.title = key.label
        config.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { attrs in
            var updated = attrs
            updated.font = UIFont.monospacedSystemFont(ofSize: 13, weight: .medium)
            return updated
        }
        config.baseBackgroundColor = UIColor.systemGray5
        config.baseForegroundColor = UIColor.label
        config.contentInsets = NSDirectionalEdgeInsets(top: 8, leading: 12, bottom: 8, trailing: 12)
        config.cornerStyle = .medium

        let button = UIButton(configuration: config)
        button.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            button.heightAnchor.constraint(greaterThanOrEqualToConstant: 44),
            button.widthAnchor.constraint(greaterThanOrEqualToConstant: 44),
        ])

        button.addAction(UIAction { [weak self] _ in
            self?.handleKey(key.kind)
        }, for: .touchUpInside)

        return button
    }

    private func handleKey(_ kind: KeyKind) {
        switch kind {
        case .esc:
            sendWithModifiers("\u{1B}", allowCtrlTransform: false)
        case .tab:
            sendWithModifiers("\t", allowCtrlTransform: false)
        case .ctrl:
            ctrlArmed.toggle()
        case .alt:
            altArmed.toggle()
        case .sequence(let sequence):
            sendWithModifiers(sequence, allowCtrlTransform: false)
        case .text(let text):
            sendWithModifiers(text, allowCtrlTransform: true)
        case .ctrlShortcut(let control):
            let scalar = UnicodeScalar(control)
            let controlChar = String(Character(scalar))
            sendWithModifiers(controlChar, allowCtrlTransform: false)
        }
    }

    private func sendWithModifiers(_ sequence: String, allowCtrlTransform: Bool) {
        var payload = sequence

        if allowCtrlTransform, ctrlArmed,
           let scalar = sequence.unicodeScalars.first, sequence.unicodeScalars.count == 1,
           scalar.value >= 0x40, scalar.value <= 0x7E,
           let control = UnicodeScalar(scalar.value & 0x1F) {
            payload = String(control)
        }

        if altArmed {
            payload = "\u{1B}" + payload
        }

        if ctrlArmed {
            ctrlArmed = false
        }
        if altArmed {
            altArmed = false
        }

        delegate?.accessory(didSend: payload)
    }

    private func updateModifierButtonAppearance() {
        updateButton(ctrlButton, armed: ctrlArmed)
        updateButton(altButton, armed: altArmed)
    }

    private func updateButton(_ button: UIButton?, armed: Bool) {
        guard let button else { return }
        var config = button.configuration
        config?.baseBackgroundColor = armed ? UIColor.systemBlue : UIColor.systemGray5
        config?.baseForegroundColor = armed ? UIColor.white : UIColor.label
        button.configuration = config
    }
}
