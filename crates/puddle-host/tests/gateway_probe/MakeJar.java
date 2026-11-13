// SPDX-License-Identifier: AGPL-3.0-or-later
// Packs a folder into a jar: `java MakeJar.java <folder> <jar>` (Gateway's runtime ships no `jar` tool).
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;

public class MakeJar {
    public static void main(String[] args) throws Exception {
        Path root = Path.of(args[0]);
        List<Path> files;
        try (var walk = Files.walk(root)) {
            files = walk.filter(Files::isRegularFile).sorted().toList();
        }
        try (OutputStream out = Files.newOutputStream(Path.of(args[1])); JarOutputStream jar = new JarOutputStream(out)) {
            for (Path file : files) {
                jar.putNextEntry(new JarEntry(root.relativize(file).toString().replace('\\', '/')));
                Files.copy(file, jar);
                jar.closeEntry();
            }
        }
    }
}
