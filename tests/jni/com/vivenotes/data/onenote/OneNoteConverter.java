package com.vivenotes.data.onenote;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;
import java.util.zip.ZipFile;

/** Exercises the exported JNI symbol, string handling, and actual conversion on a host JVM. */
public final class OneNoteConverter {
    public static native String convert(String input, String fileName, String seed, String output);

    public static void main(String[] args) throws Exception {
        System.load(Path.of(args[0]).toAbsolutePath().toString());
        Path directory = Files.createTempDirectory("vive-converter-\u7b14\u8bb0-");
        try {
            Path input = directory.resolve("incoming");
            Path output = directory.resolve("converted.vive");

            Files.copy(Path.of(args[1]), input);
            String report = convert(input.toString(), "Notes \u7b14\u8bb0.one", "seed-\u7b14\u8bb0", output.toString());
            require(!report.contains("\"error\""), report);
            require(report.contains("\"sections\":1"), report);
            require(report.contains("\"pages\":1"), report);
            try (ZipFile bundle = new ZipFile(output.toFile())) {
                require(bundle.getEntry("manifest.json") != null, "missing manifest.json");
                require(bundle.getEntry("notebook.sqlite") != null, "missing notebook.sqlite");
            }
            Files.delete(output);

            report = convert(input.toString(), "Open Notebook.onetoc2", "seed", output.toString());
            require(report.contains("\"error\":\"tableOfContents\""), report);
            require(!Files.exists(output), "a rejected input created an output file");

            Files.writeString(input, "%PDF-1.7");
            report = convert(input.toString(), "paper.pdf", "seed", output.toString());
            require(report.contains("\"error\":\"unreadable\""), report);
            require(!Files.exists(output), "an unreadable input created an output file");

            Files.delete(input);
            report = convert(input.toString(), "missing.one", "seed", output.toString());
            require(report.contains("\"error\":\"failed\""), report);
            require(!Files.exists(output), "a missing input created an output file");
            System.out.println("JNI smoke test passed");
        } finally {
            try (var files = Files.walk(directory)) {
                for (Path file : files.sorted(Comparator.reverseOrder()).toList()) {
                    Files.delete(file);
                }
            }
        }
    }

    private static void require(boolean condition, String message) throws IOException {
        if (!condition) {
            throw new IOException(message);
        }
    }
}
