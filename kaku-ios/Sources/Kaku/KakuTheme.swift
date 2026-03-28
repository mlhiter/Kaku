import SwiftUI
import SwiftTerm

// MARK: - API model

struct KakuConfigResponse: Decodable {
    let background: String?
    let foreground: String?
    let cursor_fg: String?
    let cursor_bg: String?
    let ansi: [String]
    let brights: [String]
    let font_family: String?
    let font_size: Double?
}

// MARK: - Theme

struct KakuTheme {
    var background: SwiftUI.Color = SwiftUI.Color(red: 0.12, green: 0.12, blue: 0.12)
    var foreground: SwiftUI.Color = SwiftUI.Color(red: 0.85, green: 0.85, blue: 0.85)
    var cursorColor: SwiftUI.Color = SwiftUI.Color(red: 0.5, green: 0.8, blue: 1.0)
    var ansiColors: [SwiftUI.Color] = defaultAnsi
    var brightColors: [SwiftUI.Color] = defaultBright

    // Font — uses JetBrains Mono Nerd Font for Starship icons support
    // Note: Nerd Font must be bundled in Resources/Fonts and registered in Info.plist
    var fontFamily: String = "JetBrainsMonoNerdFont-Regular"  // PostScript name
    var fontSize: CGFloat = 16

    /// Whether the background is perceptually dark (for system chrome)
    var isDark: Bool {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0
        UIColor(background).getRed(&r, green: &g, blue: &b, alpha: nil)
        // Perceived luminance (ITU-R BT.709)
        return 0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5
    }

    var colorScheme: ColorScheme { isDark ? .dark : .light }

    static let `default` = KakuTheme()

    static func from(_ resp: KakuConfigResponse) -> KakuTheme {
        var t = KakuTheme()
        if let s = resp.background { t.background  = swiftColor(s) ?? t.background }
        if let s = resp.foreground { t.foreground  = swiftColor(s) ?? t.foreground }
        if let s = resp.cursor_bg  { t.cursorColor = swiftColor(s) ?? t.cursorColor }
        t.ansiColors   = resp.ansi.compactMap(swiftColor)
        t.brightColors = resp.brights.compactMap(swiftColor)

        if let family = resp.font_family, !family.isEmpty {
            // Convert common font family names to their PostScript names
            t.fontFamily = postScriptFontName(for: family)
        }
        if let size = resp.font_size, size > 0 {
            // Desktop points → mobile points: scale up for readability
            t.fontSize = CGFloat(size) * 1.35
        }
        return t
    }

    /// Resolved UIFont: tries the configured family, falls back to system monospace.
    /// Uses a font descriptor to ensure emoji and symbol fallback works correctly.
    var uiFont: UIFont {
        guard let customFont = UIFont(name: fontFamily, size: fontSize) else {
            return UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
        }

        // Create a font descriptor that includes emoji fallback
        // This ensures starship icons and other symbols render correctly
        if let descriptor = customFont.fontDescriptor.withDesign(.monospaced) {
            return UIFont(descriptor: descriptor, size: fontSize)
        }
        return customFont
    }

    /// Creates a font with emoji fallback for the terminal
    /// Uses system font for emoji while keeping monospace for regular text
    func terminalFont(fitSize: CGFloat? = nil) -> UIFont {
        let size = fitSize ?? fontSize

        // Try custom font first - use family name if PostScript name fails
        var customFont = UIFont(name: fontFamily, size: size)
        if customFont == nil {
            // Try common variations of the font name
            let variants = [
                fontFamily,
                fontFamily.replacingOccurrences(of: "-", with: ""),
                fontFamily + "-Regular",
                "JetBrainsMono-Regular",
            ]
            for variant in variants {
                if let font = UIFont(name: variant, size: size) {
                    customFont = font
                    break
                }
            }
        }

        guard let customFont = customFont else {
            print("[KakuTheme] Failed to load custom font '\(fontFamily)', using system monospace")
            return UIFont.monospacedSystemFont(ofSize: size, weight: .regular)
        }

        print("[KakuTheme] Loaded font: \(customFont.fontName)")

        // Get the custom font's descriptor
        let customDescriptor = customFont.fontDescriptor

        // Get system font descriptor for emoji fallback
        let systemDescriptor = UIFont.systemFont(ofSize: size).fontDescriptor

        // Create cascade list: try custom font first, then system font for missing glyphs (emoji)
        let cascadeList: [[UIFontDescriptor.AttributeName: Any]] = [
            [.name: customDescriptor.postscriptName ?? fontFamily],
            [.name: systemDescriptor.postscriptName ?? ".AppleSystemUIFont"]
        ]

        // Create a new descriptor with the cascade list
        let descriptorWithFallback = customDescriptor.addingAttributes([
            .cascadeList: cascadeList
        ])

        return UIFont(descriptor: descriptorWithFallback, size: size)
    }
}

// MARK: - Hex parser

private func swiftColor(_ hex: String) -> SwiftUI.Color? {
    var s = hex.trimmingCharacters(in: .whitespaces)
    guard s.hasPrefix("#") else { return nil }
    s = String(s.dropFirst())
    guard s.count == 6, let rgb = UInt64(s, radix: 16) else { return nil }
    let r = Double((rgb >> 16) & 0xff) / 255
    let g = Double((rgb >>  8) & 0xff) / 255
    let b = Double( rgb        & 0xff) / 255
    return SwiftUI.Color(red: r, green: g, blue: b)
}

// MARK: - SwiftUI.Color → SwiftTerm.Color

extension SwiftUI.Color {
    var swiftTermColor: SwiftTerm.Color {
        guard let components = UIColor(self).cgColor.components, components.count >= 3 else {
            return SwiftTerm.Color(red: 0, green: 0, blue: 0)
        }
        return SwiftTerm.Color(
            red:   UInt16(components[0] * 65535),
            green: UInt16(components[1] * 65535),
            blue:  UInt16(components[2] * 65535)
        )
    }
}

// MARK: - Font Diagnostics

/// Lists all available monospace fonts for debugging
func listAvailableFonts() {
    let families = UIFont.familyNames.sorted()
    for family in families {
        let names = UIFont.fontNames(forFamilyName: family)
        if !names.isEmpty {
            print("[Font] Family: \(family)")
            for name in names {
                print("[Font]   - \(name)")
            }
        }
    }
}

/// Checks if a specific font is available
func isFontAvailable(_ name: String) -> Bool {
    return UIFont(name: name, size: 12) != nil
}

// MARK: - Default 16 colors (Kaku dark theme)

private let defaultAnsi: [SwiftUI.Color] = [
    SwiftUI.Color(red: 0.12, green: 0.12, blue: 0.12),
    SwiftUI.Color(red: 0.87, green: 0.34, blue: 0.34),
    SwiftUI.Color(red: 0.44, green: 0.77, blue: 0.44),
    SwiftUI.Color(red: 0.87, green: 0.73, blue: 0.34),
    SwiftUI.Color(red: 0.34, green: 0.56, blue: 0.87),
    SwiftUI.Color(red: 0.66, green: 0.34, blue: 0.87),
    SwiftUI.Color(red: 0.34, green: 0.77, blue: 0.87),
    SwiftUI.Color(red: 0.77, green: 0.77, blue: 0.77),
]

private let defaultBright: [SwiftUI.Color] = [
    SwiftUI.Color(red: 0.40, green: 0.40, blue: 0.40),
    SwiftUI.Color(red: 1.00, green: 0.50, blue: 0.50),
    SwiftUI.Color(red: 0.60, green: 0.90, blue: 0.60),
    SwiftUI.Color(red: 1.00, green: 0.90, blue: 0.50),
    SwiftUI.Color(red: 0.50, green: 0.70, blue: 1.00),
    SwiftUI.Color(red: 0.80, green: 0.50, blue: 1.00),
    SwiftUI.Color(red: 0.50, green: 0.90, blue: 1.00),
    SwiftUI.Color(red: 1.00, green: 1.00, blue: 1.00),
]

// MARK: - Font name conversion

/// Convert font family name to PostScript name for UIFont
func postScriptFontName(for family: String) -> String {
    let lower = family.lowercased()

    // JetBrains Mono Nerd Font variants
    if lower.contains("jetbrains") {
        // Try various Nerd Font naming conventions
        let variants = [
            "JetBrainsMonoNerdFont-Regular",
            "JetBrainsMono Nerd Font",
            "JetBrainsMonoNFM-Regular",
            "JetBrainsMono-Regular",
        ]
        // Return the first one that exists, or default
        for variant in variants {
            if isFontAvailable(variant) {
                print("[Font] Found available font: \(variant)")
                return variant
            }
        }
        return "JetBrainsMonoNerdFont-Regular"
    }

    // If already ends with -Regular, -Bold, etc., assume it's a PostScript name
    if family.contains("-") {
        return family
    }

    // Default: try the name as-is
    return family
}
