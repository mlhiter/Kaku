import SwiftUI

struct PaneListView: View {
    let api: KakuAPI
    let onSelect: (PaneInfo) -> Void

    @State private var panes: [PaneInfo] = []
    @State private var error: String?
    @State private var loading = true

    var body: some View {
        Group {
            if loading {
                ProgressView("Loading panes...")
            } else if let error {
                VStack(spacing: 12) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.largeTitle)
                        .foregroundStyle(.orange)
                    Text(error).multilineTextAlignment(.center)
                    Button("Retry") { Task { await load() } }
                }
            } else if panes.isEmpty {
                Text("No active panes found.\nOpen a tab in Kaku first.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
            } else {
                List(panes) { pane in
                    Button {
                        onSelect(pane)
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(pane.title)
                                .font(.headline)
                            if let cwd = pane.cwd {
                                Text(cwd)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(.vertical, 4)
                    }
                    .foregroundStyle(.primary)
                }
            }
        }
        .navigationTitle("Select Pane")
        .task { await load() }
        .refreshable { await load() }
    }

    private func load() async {
        loading = true
        error = nil
        do {
            panes = try await api.fetchPanes()
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }
}
