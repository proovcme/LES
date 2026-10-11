import Foundation
import Vision
import ImageIO

if CommandLine.arguments.count != 2 { exit(2) }
do {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = true
    request.automaticallyDetectsLanguage = true
    let supported = try request.supportedRecognitionLanguages()
    request.recognitionLanguages = ["ru-RU", "en-US"].filter { supported.contains($0) }
    let handler = VNImageRequestHandler(url: URL(fileURLWithPath: CommandLine.arguments[1]), options: [:])
    try handler.perform([request])
    let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
    let data = try JSONSerialization.data(withJSONObject: ["text": lines.joined(separator: "\n")])
    FileHandle.standardOutput.write(data)
} catch { FileHandle.standardError.write(Data("Vision OCR failed: \(error)".utf8)); exit(1) }
