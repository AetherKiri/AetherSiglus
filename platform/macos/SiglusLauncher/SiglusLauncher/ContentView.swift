import SwiftUI
import AppKit

struct ContentView: View {
    @EnvironmentObject var library: GameLibrary

    private let columns: [GridItem] = [
        GridItem(.flexible(), spacing: 16),
        GridItem(.flexible(), spacing: 16),
        GridItem(.flexible(), spacing: 16),
    ]

    var body: some View {
        VStack(spacing: 12) {
            header
            Divider()
            ScrollView(.vertical) {
                LazyVGrid(columns: columns, spacing: 16) {
                    ForEach(library.games) { game in
                        GameTileView(game: game)
                    }
                }
                .padding(16)
            }
        }
        .frame(minWidth: 920, minHeight: 560)
        .alert(isPresented: $library.showError) {
            Alert(
                title: Text("Error"),
                message: Text(library.errorMessage),
                dismissButton: .default(Text("OK"))
            )
        }
        .sheet(item: $library.pendingImport) { p in
            ImportNlsSheet(pending: p)
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            Text("Siglus")
                .font(.title2)
                .bold()

            Spacer()

            Button("Import…") {
                library.importGameFolder()
            }
            .keyboardShortcut("i", modifiers: [.command])

            Button("Refresh") {
                library.refreshValidation()
            }
            .keyboardShortcut("r", modifiers: [.command])
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
    }
}

struct ImportNlsSheet: View {
    @EnvironmentObject var library: GameLibrary
    let pending: PendingImport

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Import Game")
                .font(.headline)

            Text(pending.title)
                .font(.subheadline)
                .foregroundColor(.secondary)

            HStack {
                Spacer()
                Button("Cancel") {
                    library.pendingImport = nil
                }
                Button("Import") {
                    library.commitImport(p: pending)
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 420)
    }
}

struct GameTileView: View {
    @EnvironmentObject var library: GameLibrary
    let game: GameEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .topTrailing) {
                RoundedRectangle(cornerRadius: 12)
                    .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 1)
                    .background(RoundedRectangle(cornerRadius: 12).fill(Color.secondary.opacity(0.08)))

                if let cover = library.loadCoverImage(game: game) {
                    Image(nsImage: cover)
                        .resizable()
                        .scaledToFill()
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                } else {
                    Text(game.title)
                        .font(.headline)
                        .multilineTextAlignment(.center)
                        .padding(12)
                }

            }
            .frame(height: 160)
            .clipped()

            Text(game.title)
                .font(.headline)
                .lineLimit(2)

            Text(game.rootPath)
                .font(.caption)
                .foregroundColor(.secondary)
                .lineLimit(1)

            HStack {
                Button("Play") {
                    library.launch(game: game)
                }
                .keyboardShortcut(.defaultAction)

                Spacer()

                Menu {
                    Button("Reveal in Finder") {
                        library.revealInFinder(game: game)
                    }
                    Button("Remove from Library") {
                        library.remove(game: game)
                    }
                } label: {
                    Text("…")
                        .font(.headline)
                        .frame(width: 28, height: 22)
                }
                .menuStyle(.borderlessButton)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 16).fill(Color(NSColor.windowBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(Color.secondary.opacity(0.20)))
    }
}
