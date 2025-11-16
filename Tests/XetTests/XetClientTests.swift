import Foundation
import Testing

#if canImport(Xet)
    import Xet
#else
    #warning(
        "Xet module could not be found. Have you generated UniFFI bindings? Run './Scripts/generate-bindings.sh' before running tests."
    )
#endif

@Suite struct XetClientTests {
    @Test("Xet client initialization")
    func xetInitialization() async throws {
        let xet = try XetClient()
        let version = xet.version()
        #expect(!version.isEmpty)
        #expect(version == "0.1.0")
    }

    @Test("Xet client initialization with token")
    func xetWithToken() async throws {
        let xet = try XetClient.withToken(token: "test-token")
        let version = xet.version()
        #expect(!version.isEmpty)
    }

    @Test("Download file via Xet CAS")
    func downloadFileViaXet() async throws {
        let xet = try XetClient()

        let knownHash = "6aec39639a0a2d1ca966356b8c2b8426a484f80ff80731f44fa8482040713bdf"
        let knownSize: UInt64 = 11_422_654
        let fileInfo =
            try xet.getFileInfo(
                repo: "Qwen/Qwen3-0.6B",
                path: "tokenizer.json",
                revision: "main"
            ) ?? XetFileInfo(hash: knownHash, fileSize: knownSize)

        let jwt = try xet.getCasJwt(
            repo: "Qwen/Qwen3-0.6B",
            revision: "main",
            isUpload: false
        )

        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }

        let downloads = try xet.downloadFiles(
            fileInfos: [fileInfo],
            destinationDir: tempDir.path,
            jwtInfo: jwt
        )

        #expect(downloads.count == 1)

        let resultPath = downloads[0]
        let attrs = try FileManager.default.attributesOfItem(atPath: resultPath)
        let sizeOnDisk = (attrs[.size] as? NSNumber)?.uint64Value
        #expect(sizeOnDisk == fileInfo.fileSize())
    }

    @Test("XetFileInfo creation and serialization")
    func xetFileInfo() async throws {
        let fileInfo = XetFileInfo(hash: "abc123", fileSize: 1024)

        #expect(fileInfo.hash() == "abc123")
        #expect(fileInfo.fileSize() == 1024)
    }
}
