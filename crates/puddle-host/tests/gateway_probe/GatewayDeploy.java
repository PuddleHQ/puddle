package puddle.probe;

import com.intellij.openapi.progress.EmptyProgressIndicator;
import com.intellij.openapi.progress.ProgressIndicator;
import com.intellij.remote.AuthType;
import com.intellij.remote.RemoteCredentials;
import com.intellij.ssh.ConnectionBuilder;
import com.intellij.ssh.config.unified.SshConfig;
import com.jetbrains.gateway.ssh.DeployFlowUtil;
import com.jetbrains.gateway.ssh.HighLevelHostAccessor;
import com.jetbrains.gateway.ssh.HostDeployInputs;
import com.jetbrains.gateway.ssh.SshDeployFlowUtil;
import com.jetbrains.gateway.ssh.SshHostTunnelConnector;
import com.jetbrains.gateway.ssh.SshMultistagePanelContext;
import kotlin.coroutines.EmptyCoroutineContext;
import kotlinx.coroutines.BuildersKt;

/**
 * Gateway's own bootstrap, the steps its "Connect via SSH" flow runs before it picks an IDE: connect with an
 * "OpenSSH config and authentication agent" connection, check the host and deploy its remote-dev-worker, then the
 * host checks (memory, CPUs, file locking, disk space, cache and config dirs, a port forwarding test).
 */
final class GatewayDeploy {
    @SuppressWarnings("rawtypes")
    interface Suspend {
        Object call(kotlin.coroutines.Continuation cont) throws Exception;
    }

    @SuppressWarnings({"unchecked", "rawtypes"})
    static Object await(Suspend body) throws Exception {
        return BuildersKt.runBlocking(EmptyCoroutineContext.INSTANCE, (scope, cont) -> {
            try {
                return body.call((kotlin.coroutines.Continuation) cont);
            } catch (RuntimeException e) {
                throw e;
            } catch (Exception e) {
                throw new RuntimeException(e);
            }
        });
    }

    static void run(ProbeStarter.StepSink sink, ConnectionBuilder cb, String alias, String user) {
        ProgressIndicator indicator = new EmptyProgressIndicator();
        SshConfig config = SshConfig.create(false, alias, 22, user, AuthType.OPEN_SSH, null);
        RemoteCredentials credentials = config.copyToCredentials();
        SshMultistagePanelContext context = new SshMultistagePanelContext(new HostDeployInputs.WithHostInfo(credentials));
        HighLevelHostAccessor[] accessor = new HighLevelHostAccessor[1];
        sink.step("gw-deploy-worker", () -> {
            Object result = await(cont -> DeployFlowUtil.INSTANCE.checkHostAndDeployWorker(indicator, context, cont));
            accessor[0] = context.getHostAccessor();
            return "result=" + result + " data=" + context.getDeployData();
        });
        if (accessor[0] == null) return;
        HighLevelHostAccessor a = accessor[0];
        sink.step("gw-selfcheck", () -> String.valueOf(await(cont -> a.selfcheck(cont))));
        sink.step("gw-guess-os", () -> String.valueOf(await(cont -> a.guessOs(cont))));
        sink.step("gw-home", () -> String.valueOf(await(cont -> a.getHomeDir(cont))));
        sink.step("gw-env", () -> String.valueOf(await(cont -> a.getEnv(cont))));
        sink.step("gw-can-download", () -> String.valueOf(await(cont -> a.canDownload(cont))));
        sink.step("gw-check-memory",
                () -> String.valueOf(await(cont -> SshDeployFlowUtil.INSTANCE.checkAvailableMemory(indicator, a, cont))));
        sink.step("gw-check-cpus",
                () -> String.valueOf(await(cont -> SshDeployFlowUtil.INSTANCE.checkCPUCount(indicator, a, cont))));
        sink.step("gw-check-file-locking", () -> String.valueOf(
                await(cont -> SshDeployFlowUtil.INSTANCE.checkFileLockingSupport(indicator, a, cont))));
        sink.step("gw-check-space",
                () -> String.valueOf(await(cont -> SshDeployFlowUtil.INSTANCE.checkAvailableSpace(indicator, a, cont))));
        sink.step("gw-prepare-dirs", () -> String.valueOf(
                await(cont -> SshDeployFlowUtil.INSTANCE.prepareCacheAndConfigDirectories(indicator, a, cont))));
        sink.step("gw-check-port-forwarding", () -> String.valueOf(await(cont -> SshDeployFlowUtil.INSTANCE
                .checkPortForwarding(indicator, a, new SshHostTunnelConnector(credentials), cont))));
        sink.step("gw-installed-ides", () -> String.valueOf(await(cont -> a.getInstalledIDEs(false, cont))));
    }
}
