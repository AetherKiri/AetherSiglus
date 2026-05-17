import Foundation
import SwiftUI
import UIKit
import Darwin

@_silgen_name("siglus_game_name_from_dir")
private func siglus_game_name_from_dir(_ gameRootUtf8: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?

@_silgen_name("siglus_game_cover_path_from_dir")
private func siglus_game_cover_path_from_dir(_ gameRootUtf8: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?

@_silgen_name("siglus_string_free")
private func siglus_string_free(_ ptr: UnsafeMutablePointer<CChar>?) -> Void

private func takeSiglusString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr else { return nil }
    let out = String(cString: ptr)
    siglus_string_free(ptr)
    let trimmed = out.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? nil : trimmed
}

private func siglusGameName(for dir: URL) -> String {
    return dir.path.withCString { cPath in
        takeSiglusString(siglus_game_name_from_dir(cPath))
    } ?? dir.lastPathComponent
}

private func siglusGameCoverPath(for dir: URL) -> String? {
    return dir.path.withCString { cPath in
        takeSiglusString(siglus_game_cover_path_from_dir(cPath))
    }
}

struct GameEntry: Identifiable, Codable, Equatable {
    let id: String
    var title: String
    var rootPath: String

    var addedAtUnix: Int64
    var lastPlayedAtUnix: Int64?
    var coverPath: String?

    init(
        id: String,
        title: String,
        rootPath: String,
        addedAtUnix: Int64,
        lastPlayedAtUnix: Int64? = nil,
        coverPath: String? = nil
    ) {
        self.id = id
        self.title = title
        self.rootPath = rootPath
        self.addedAtUnix = addedAtUnix
        self.lastPlayedAtUnix = lastPlayedAtUnix
        self.coverPath = coverPath
    }

    enum CodingKeys: String, CodingKey {
        case id
        case title
        case rootPath
        case addedAtUnix
        case lastPlayedAtUnix
        case coverPath
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decode(String.self, forKey: .title)
        rootPath = try c.decode(String.self, forKey: .rootPath)
        addedAtUnix = try c.decode(Int64.self, forKey: .addedAtUnix)
        lastPlayedAtUnix = try c.decodeIfPresent(Int64.self, forKey: .lastPlayedAtUnix)
        coverPath = try c.decodeIfPresent(String.self, forKey: .coverPath)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(title, forKey: .title)
        try c.encode(rootPath, forKey: .rootPath)
        try c.encode(addedAtUnix, forKey: .addedAtUnix)
        try c.encodeIfPresent(lastPlayedAtUnix, forKey: .lastPlayedAtUnix)
        try c.encodeIfPresent(coverPath, forKey: .coverPath)
    }
}

final class GameLibrary: ObservableObject {
    @Published var games: [GameEntry] = []
    @Published var showError: Bool = false
    @Published var errorMessage: String = ""

    // When non-nil, present the in-app player (iOS host-mode).
    @Published var activeGame: GameEntry? = nil

    private let fm = FileManager.default

    // MARK: - Storage (settings only)
    private var appSupportDir: URL {
        let base = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let dir = base.appendingPathComponent("SiglusLauncher", isDirectory: true)
        if !fm.fileExists(atPath: dir.path) {
            try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        return dir
    }

    // Games live in Documents/siglus so the user can copy folders in via the Files app.
    private var documentsDir: URL {
        fm.urls(for: .documentDirectory, in: .userDomainMask).first!
    }

    private var documentsGamesDir: URL {
        let dir = documentsDir.appendingPathComponent("siglus", isDirectory: true)
        if !fm.fileExists(atPath: dir.path) {
            try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        return dir
    }

    private var libraryURL: URL {
        appSupportDir.appendingPathComponent("library.json")
    }

    init() {
        // Ensure the Files-visible folder exists as early as possible.
        _ = documentsGamesDir
        load()
    }

    func load() {
        do {
            if fm.fileExists(atPath: libraryURL.path) {
                let data = try Data(contentsOf: libraryURL)
                games = try JSONDecoder().decode([GameEntry].self, from: data)
            } else {
                games = []
            }
        } catch {
            games = []
        }
        // Always rebuild the list from Documents/siglus.
        rescanFromDocuments()
    }

    func save() {
        do {
            let data = try JSONEncoder().encode(games)
            try data.write(to: libraryURL, options: [.atomic])
        } catch {
            // best-effort
        }
    }

    // MARK: - Scan games in Documents/siglus
    func rescanFromDocuments() {
        // Preserve per-game settings (last played, etc.) from library.json.
        let savedById: [String: GameEntry] = Dictionary(uniqueKeysWithValues: games.map { ($0.id, $0) })
        var out: [GameEntry] = []

        let now = Int64(Date().timeIntervalSince1970)

        let root = documentsGamesDir
        guard let items = try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey], options: [.skipsHiddenFiles]) else {
            games = []
            save()
            return
        }

        for url in items {
            let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if !isDir { continue }

            let gameRoot = url
            let id = stableId(for: gameRoot.path)

            let saved = savedById[id]
            let title = siglusGameName(for: gameRoot)
            let addedAt = saved?.addedAtUnix ?? now
            let lastPlayed = saved?.lastPlayedAtUnix
            let coverPath = siglusGameCoverPath(for: gameRoot)

            out.append(GameEntry(id: id, title: title, rootPath: gameRoot.path, addedAtUnix: addedAt, lastPlayedAtUnix: lastPlayed, coverPath: coverPath))
        }

        // Stable-ish ordering: recently played first, then newest.
        out.sort { a, b in
            let ap = a.lastPlayedAtUnix ?? 0
            let bp = b.lastPlayedAtUnix ?? 0
            if ap != bp { return ap > bp }
            return a.addedAtUnix > b.addedAtUnix
        }

        games = out
        save()
    }
    func remove(game: GameEntry) {
        // Remove from library and delete the game folder (Documents/siglus/...)
        games.removeAll { $0.id == game.id }
        save()

        // Best-effort: remove the folder pointed by rootPath.
        let root = URL(fileURLWithPath: game.rootPath)
        try? fm.removeItem(at: root)
    }

    // MARK: - Launch
    func launch(game: GameEntry) {
        if let idx = games.firstIndex(of: game) {
            games[idx].lastPlayedAtUnix = Int64(Date().timeIntervalSince1970)
            save()
        }
        // Present the in-app player (SwiftUI owns the main loop).
        activeGame = game
    }

    // MARK: - Helpers
    private func cleanup(url: URL) {
        try? fm.removeItem(at: url)
    }

    func loadCoverImage(game: GameEntry) -> UIImage? {
        guard let coverPath = game.coverPath, !coverPath.isEmpty else { return nil }
        return UIImage(contentsOfFile: coverPath)
    }

    private func stableId(for path: String) -> String {
        // Stable enough for local library usage.
        return String(path.hashValue, radix: 16)
    }

    private func looksLikeGameRoot(_ dir: URL) -> Bool {
        let gameexeDat = dir.appendingPathComponent("Gameexe.dat")
        if fm.fileExists(atPath: gameexeDat.path) { return true }
        let gameexeIni = dir.appendingPathComponent("Gameexe.ini")
        if fm.fileExists(atPath: gameexeIni.path) { return true }
        let scenePck = dir.appendingPathComponent("scene.pck")
        if fm.fileExists(atPath: scenePck.path) { return true }
        let dataDir = dir.appendingPathComponent("data", isDirectory: true)
        if fm.fileExists(atPath: dataDir.path) { return true }
        return true
    }

    private func showError(_ msg: String) {
        errorMessage = msg
        showError = true
    }
}
