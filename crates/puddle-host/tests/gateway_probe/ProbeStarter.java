package puddle.probe;

import com.intellij.openapi.application.ApplicationStarter;
import com.intellij.ssh.ConnectionBuilder;
import com.intellij.ssh.SshSession;
import com.intellij.ssh.channels.SftpChannel;
import com.intellij.ssh.process.SshExecProcess;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.PrintStream;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.List;
import java.util.Random;
import java.util.concurrent.TimeUnit;

/**
 * Headless probe: drives Gateway's own SSH stack (IntelliJ platform ConnectionBuilder, sshj, OpenSSH config through
 * `ssh -G`, IntelliJ's ProxyCommand runner and host key verifier) against one host alias.
 *
 * gateway.sh puddleProbe <alias> <report file> <guest tcp port> [steps]
 */
public final class ProbeStarter implements ApplicationStarter {
    private PrintStream report;
    private int failures;

    @Override
    public boolean isHeadless() {
        return true;
    }

    @Override
    public void main(List<String> args) {
        int code;
        try {
            report = new PrintStream(Files.newOutputStream(Path.of(args.get(2))), true, StandardCharsets.UTF_8);
            Thread worker = new Thread(() -> run(args.get(1), Integer.parseInt(args.get(3)),
                    args.size() > 4 ? args.get(4) : "ssh"));
            worker.start();
            worker.join();
            code = failures == 0 ? 0 : 1;
        } catch (Throwable t) {
            t.printStackTrace();
            code = 2;
        }
        if (report != null) {
            report.println("DONE failures=" + failures);
            report.close();
        }
        System.exit(code);
    }

    private void say(String line) {
        report.println(line);
        System.out.println("PROBE " + line);
    }

    interface Step {
        String run() throws Exception;
    }

    private void step(String name, Step body) {
        long start = System.nanoTime();
        say("START " + name);
        try {
            String detail = body.run();
            say("OK " + name + " (" + ms(start) + " ms) " + oneLine(detail));
        } catch (Throwable t) {
            failures++;
            say("FAIL " + name + " (" + ms(start) + " ms) " + oneLine(t.toString()));
            Throwable c = t.getCause();
            while (c != null) {
                say("  cause " + oneLine(c.toString()));
                c = c.getCause();
            }
            t.printStackTrace(report);
        }
    }

    private static long ms(long start) {
        return TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - start);
    }

    private static String oneLine(String s) {
        if (s == null) return "";
        s = s.replace("\r", "").replace("\n", " | ");
        return s.length() > 2000 ? s.substring(0, 2000) + "..." : s;
    }

    private static String readAll(InputStream in) throws Exception {
        return new String(in.readAllBytes(), StandardCharsets.UTF_8);
    }

    private static final class Exec {
        int code;
        String out;
        String err;
    }

    private static Exec exec(ConnectionBuilder cb, String command, byte[] stdin) throws Exception {
        SshExecProcess p = cb.execBuilder(command).execute();
        if (stdin != null) {
            try (OutputStream o = p.getOutputStream()) {
                o.write(stdin);
            }
        } else {
            p.getOutputStream().close();
        }
        Exec e = new Exec();
        ByteArrayOutputStream errBuf = new ByteArrayOutputStream();
        Thread errReader = new Thread(() -> {
            try {
                p.getErrorStream().transferTo(errBuf);
            } catch (Exception ignored) {
            }
        });
        errReader.start();
        e.out = readAll(p.getInputStream());
        if (!p.waitFor(120, TimeUnit.SECONDS)) throw new IllegalStateException("no exit in 120 s: " + command);
        errReader.join(10_000);
        e.err = errBuf.toString(StandardCharsets.UTF_8);
        e.code = p.exitValue();
        return e;
    }

    private void run(String alias, int guestPort, String steps) {
        String ssh = System.getenv("PUDDLE_PROBE_SSH");
        if (ssh != null) {
            // The one test seam: Gateway's own "ssh executable" advanced setting points at a wrapper that adds
            // `-F <test config>`, because OpenSSH reads ~/.ssh/config from the passwd entry, not from $HOME.
            com.intellij.openapi.options.advanced.AdvancedSettings.Companion.setString("ssh.tool.custom.path", ssh);
            say("INFO ssh.tool.custom.path=" + ssh);
        }
        ConnectionBuilder cb = new ConnectionBuilder(alias)
                .withParsingOpenSSHConfig(true)
                .withSshPasswordProvider(new com.intellij.ssh.interaction.SshPasswordProvider() {
                    // Puddle's entry needs no secret: any prompt is a failure, recorded in the report.
                    @Override
                    public String getKeyPassphrase(Path key, com.intellij.openapi.progress.ProgressIndicator i) {
                        say("PROMPT key passphrase for " + key);
                        return null;
                    }

                    @Override
                    public String getUnixPassword(com.intellij.openapi.progress.ProgressIndicator i) {
                        say("PROMPT unix password");
                        return null;
                    }

                    @Override
                    public String[] getKeyboardInteractive(String name, String instruction, String[] prompt,
                            boolean[] echo, com.intellij.openapi.progress.ProgressIndicator i) {
                        say("PROMPT keyboard-interactive " + String.join(",", prompt));
                        return null;
                    }
                })
                .withConnectionTimeout(60, TimeUnit.SECONDS);
        step("config", () -> cb.buildConnectionConfig().toString());
        SshSession[] session = new SshSession[1];
        step("connect", () -> {
            session[0] = cb.connect();
            return "user=" + session[0].getUserName() + " host=" + session[0].getHost() + " backend="
                    + session[0].getSshBackendLibrary();
        });
        if (session[0] == null) return;
        step("exec-identity", () -> {
            Exec e = exec(cb, "id -un; echo \"$HOME\"; uname -sm; sudo -n id -un; echo shell=$SHELL; getent passwd $(id -un); command -v python3 busybox nc setsid", null);
            if (e.code != 0) throw new IllegalStateException("exit " + e.code + " err=" + e.err);
            return e.out;
        });
        step("exec-exit-code-7", () -> {
            Exec e = exec(cb, "echo REMOTE_EXEC_OUTPUT_MARKER_ && exit 7", null);
            if (e.code != 7) throw new IllegalStateException("exit " + e.code + " out=" + e.out + " err=" + e.err);
            return e.out;
        });
        step("exec-stdin-1MiB", () -> {
            byte[] data = new byte[1 << 20];
            new Random(1).nextBytes(data);
            String want = sha(data);
            Exec e = exec(cb, "sha256sum | cut -d' ' -f1", data);
            if (e.code != 0 || !e.out.trim().equals(want))
                throw new IllegalStateException("exit " + e.code + " got " + e.out.trim() + " want " + want);
            return want;
        });
        step("exec-stderr", () -> {
            Exec e = exec(cb, "echo to-stderr >&2; echo to-stdout", null);
            if (!e.err.contains("to-stderr") || !e.out.contains("to-stdout"))
                throw new IllegalStateException("out=" + e.out + " err=" + e.err);
            return "separate streams";
        });
        step("sftp", () -> {
            SftpChannel sftp = cb.openSftpChannel();
            String home = sftp.getHome();
            if (!home.startsWith("/home/")) say("INFO sftp home is " + home + ", using the exec home instead");
            String dir = home.startsWith("/home/") ? home : exec(cb, "echo $HOME", null).out.trim();
            String path = dir + "/.puddle-gateway-probe";
            byte[] data = new byte[8 << 20];
            new Random(2).nextBytes(data);
            try (OutputStream o = sftp.outputStream(path, false)) {
                o.write(data);
            }
            long size = sftp.stat(path).getSize();
            byte[] back;
            try (InputStream i = sftp.inputStream(path)) {
                back = i.readAllBytes();
            }
            int listed = sftp.ls(dir).size();
            sftp.rm(path);
            if (size != data.length || !sha(back).equals(sha(data)))
                throw new IllegalStateException("size " + size + " sha " + sha(back));
            return "home=" + home + " 8 MiB up and back, ls " + listed + " entries";
        });
        step("detached-listener", () -> {
            // A process started in the background must outlive its exec channel (Gateway starts its backend so).
            String server = "command -v python3 >/dev/null && exec python3 -m http.server " + guestPort
                    + " --bind 127.0.0.1; exec busybox httpd -f -p 127.0.0.1:" + guestPort + " -h /tmp";
            Exec e = exec(cb, "cd /tmp && nohup sh -c '" + server + "' >/tmp/probe-listener.log 2>&1 </dev/null & echo started", null);
            Thread.sleep(3000);
            Exec log = exec(cb, "cat /tmp/probe-listener.log; ps -eo pid,ppid,args | grep -E 'http' | grep -v grep", null);
            say("INFO listener: " + oneLine(log.out));
            Thread.sleep(3000);
            Exec probe = exec(cb, "(ss -ltn 2>/dev/null || netstat -ltn 2>/dev/null || cat /proc/net/tcp) | head -20", null);
            return e.out + " | " + probe.out;
        });
        step("local-tunnel", () -> {
            int local = session[0].addLocalTunnelWithRandomLocalPort("127.0.0.1", guestPort);
            StringBuilder got = new StringBuilder();
            for (int round = 0; round < 3; round++) {
                try (Socket s = new Socket("127.0.0.1", local)) {
                    s.setSoTimeout(30_000);
                    s.getOutputStream().write("GET / HTTP/1.0\r\nHost: x\r\n\r\n".getBytes(StandardCharsets.US_ASCII));
                    s.getOutputStream().flush();
                    String reply = readAll(s.getInputStream());
                    if (!reply.startsWith("HTTP/")) throw new IllegalStateException("reply: " + reply);
                    got.append(reply, 0, Math.min(reply.length(), 15)).append(';');
                }
            }
            session[0].removeLocalTunnel(local);
            return "local " + local + " -> guest 127.0.0.1:" + guestPort + " " + got;
        });
        if (steps.contains("deploy")) {
            GatewayDeploy.run(this::step, cb, alias, session[0].getUserName());
        }
        session[0].disconnect();
    }

    interface StepSink {
        void step(String name, Step body);
    }

    static String sha(byte[] data) throws Exception {
        return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(data));
    }
}
