import haven.AuthClient;
import haven.Charlist;
import haven.Coord;
import haven.Equipory;
import haven.HackThread;
import haven.MainFrame;
import haven.MapView;
import haven.Message;
import haven.Resource;
import haven.RemoteUI;
import haven.Session;
import haven.SlenHud;
import haven.UI;
import haven.Widget;

import java.net.InetAddress;
import java.util.Map;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Headless client-side probe: drives the REAL client classes (AuthClient,
 * Session, UI, RemoteUI - the exact post-login receive path, no GL) against
 * a live server. Any throwable raised by widget creation or message parsing
 * is exactly what kills the real client's threads and produces the reported
 * "black screen after entering the world".
 *
 * Usage: java UiProbe [user] [password] [run|equip]
 * Prints "UI PROBE: OK" (or "UI PROBE EQUIP: OK" in equip mode, which also
 * requires the Equipory paperdoll widget to exist) and exits 0 on success.
 */
public class UiProbe {
    static void fail(String msg) {
        System.out.println("UI PROBE FAIL: " + msg);
        System.exit(1);
    }

    public static void main(String[] args) throws Exception {
        // AuthClient/Session open HackSockets, which refuse to connect from
        // a plain thread (HackSocket.hook) - run on a HackThread like the
        // real client's MainFrame does.
        final Throwable[] err = new Throwable[1];
        HackThread t = new HackThread(() -> {
            try {
                run(args);
            } catch (Throwable e) {
                err[0] = e;
            }
        }, "probe-main");
        t.start();
        t.join();
        if (err[0] != null) {
            err[0].printStackTrace(System.out);
            fail("probe error: " + err[0]);
        }
    }

    static void run(String[] args) throws Exception {
        String user = args.length > 0 ? args[0] : "probeuser";
        String pass = args.length > 1 ? args[1] : "probepass";
        String mode = args.length > 2 ? args[2] : "run";

        // The widget factories read these statics; normally set by MainFrame.
        MainFrame.innerSize = new java.awt.Dimension(800, 600);
        MainFrame.centerPoint = new java.awt.Point(400, 300);
        // MapView's lighting mask reads the screen size static.
        MainFrame.screenSZ = new Coord(800, 600);
        // MainFrame.run() chains the HTTP resource source; the probe must do
        // the same (dev res server on 1872, like the real client layout).
        Resource.addurl(new java.net.URL("http://127.0.0.1:1872/"));

        Thread.setDefaultUncaughtExceptionHandler((t, e) -> {
            System.out.println("PROBE-UNCAUGHT thread=" + t.getName() + ": " + e);
            e.printStackTrace(System.out);
        });

        AuthClient auth = new AuthClient("127.0.0.1", user);
        if (!auth.trypasswd(pass))
            fail("auth rejected");
        byte[] cookie = auth.cookie;
        auth.close();

        Session sess = new Session(InetAddress.getByName("127.0.0.1"), user, cookie);
        long deadline = System.currentTimeMillis() + 10000;
        while (System.currentTimeMillis() < deadline) {
            if ("".equals(sess.state))
                break;
            if (sess.connfailed != 0)
                fail("connfailed=" + sess.connfailed);
            Thread.sleep(50);
        }
        if (!"".equals(sess.state))
            fail("session not connected; state=" + sess.state);

        UI ui = new UI(new Coord(800, 600), sess);
        RemoteUI rui = new RemoteUI(sess);
        final AtomicReference<Throwable> ruiDeath = new AtomicReference<>();
        Thread rt = new Thread(() -> {
            try {
                rui.run(ui);
            } catch (Throwable e) {
                ruiDeath.set(e);
            }
        }, "probe-remoteui");
        rt.setDaemon(true);
        rt.start();

        // Wait for the charlist widget (login flow part 1).
        deadline = System.currentTimeMillis() + 15000;
        int clid = -1;
        while (System.currentTimeMillis() < deadline && clid < 0) {
            clid = findWidget(ui, Charlist.class);
            if (clid < 0)
                Thread.sleep(100);
        }
        if (clid < 0)
            fail("charlist never appeared");
        System.out.println("PROBE: charlist id=" + clid);

        // Select the character exactly like the real client does.
        Message play = new Message(Message.RMSG_WDGMSG);
        play.adduint16(clid);
        play.addstring("play");
        play.addlist(new Object[] { "Player" });
        sess.queuemsg(play);

        // Wait for the world widget tree (login flow part 2). This is the
        // window where the reported black screen happens.
        deadline = System.currentTimeMillis() + 20000;
        while (System.currentTimeMillis() < deadline) {
            Throwable d = ruiDeath.get();
            if (d != null) {
                d.printStackTrace(System.out);
                fail("remoteui thread died: " + d);
            }
            if ("dead".equals(sess.state))
                fail("session reader died (message parse exception?)");
            if (findWidget(ui, MapView.class) >= 0
                    && findWidget(ui, SlenHud.class) >= 0)
                break;
            Thread.sleep(100);
        }
        int mvid = findWidget(ui, MapView.class);
        if (mvid < 0)
            fail("mapview never appeared (black screen reproduced)");
        System.out.println("PROBE: mapview id=" + mvid);

        // Soak: six seconds of live traffic; the client path must survive
        // MAPDATA, OBJDATA, GLOBLOB, movement and visibility updates.
        long soak = System.currentTimeMillis() + 6000;
        while (System.currentTimeMillis() < soak) {
            Throwable d = ruiDeath.get();
            if (d != null) {
                d.printStackTrace(System.out);
                fail("remoteui died during soak: " + d);
            }
            if (!"".equals(sess.state))
                fail("session state changed to: " + sess.state);
            Thread.sleep(100);
        }

        StringBuilder sb = new StringBuilder();
        synchronized (ui.widgets) {
            for (Map.Entry<Integer, Widget> e : ui.widgets.entrySet())
                sb.append(e.getKey()).append(':')
                        .append(e.getValue().getClass().getSimpleName())
                        .append(' ');
        }
        System.out.println("PROBE WIDGETS: " + sb);

        if (mode.equals("equip")) {
            int eq = findWidget(ui, Equipory.class);
            if (eq < 0)
                fail("no Equipory (paperdoll) widget after world entry");
            System.out.println("PROBE: equipory id=" + eq);
            System.out.println("UI PROBE EQUIP: OK");
        } else {
            System.out.println("UI PROBE: OK");
        }
        System.exit(0);
    }

    static int findWidget(UI ui, Class<?> c) {
        synchronized (ui.widgets) {
            for (Map.Entry<Integer, Widget> e : ui.widgets.entrySet())
                if (c.isInstance(e.getValue()))
                    return e.getKey();
        }
        return -1;
    }
}
