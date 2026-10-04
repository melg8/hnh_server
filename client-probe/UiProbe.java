import haven.AuthClient;
import haven.Button;
import haven.Charlist;
import haven.Coord;
import haven.Equipory;
import haven.HackThread;
import haven.Indir;
import haven.MainFrame;
import haven.MapView;
import haven.Message;
import haven.Resource;
import haven.RemoteUI;
import haven.Session;
import haven.SlenHud;
import haven.UI;
import haven.Widget;

import java.lang.reflect.Field;
import java.net.InetAddress;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Headless client-side probe: drives the REAL client classes (AuthClient,
 * Session, UI, RemoteUI - the exact post-login receive path, no GL) against
 * a live server. Any throwable raised by widget creation or message parsing
 * is exactly what kills the real client's threads and produces the reported
 * "black screen after entering the world".
 *
 * Usage: java UiProbe [user] [password] [run|equip|charlist]
 * Prints "UI PROBE: OK" (or "UI PROBE EQUIP: OK" in equip mode, which also
 * requires the Equipory paperdoll widget to exist) and exits 0 on success.
 *
 * Mode "charlist" proves the character-selection screen end to end: the
 * charlist "add" uimsg decoded into a real Char entry, every avatar layer
 * resource resolves client-side (the login portrait data path), the
 * composited layer inventory is non-empty, and world entry happens through
 * the REAL Button.click() chain instead of a raw queued "play" message.
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

    static Object field(Object o, String name) throws Exception {
        for (Class<?> c = o.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                return f.get(o);
            } catch (NoSuchFieldException e) {
                // Walk up the hierarchy.
            }
        }
        throw new NoSuchFieldException(name + " on " + o.getClass());
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

        if (mode.equals("charlist"))
            runCharlist(ui, clid);
        else {
            // Select the character exactly like the real client does.
            Message play = new Message(Message.RMSG_WDGMSG);
            play.adduint16(clid);
            play.addstring("play");
            play.addlist(new Object[] { "Player" });
            sess.queuemsg(play);
        }

        // Wait for the world widget tree (login flow part 2). This is the
        // window where the reported black screen happens.
        waitForWorld(ui, ruiDeath);

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
        } else if (mode.equals("charlist")) {
            System.out.println("UI PROBE CHARLIST: OK");
        } else {
            System.out.println("UI PROBE: OK");
        }
        System.exit(0);
    }

    /** Charlist-mode proof: portrait data path + real click chain. */
    static void runCharlist(UI ui, int clid) throws Exception {
        Widget cl = ui.widgets.get(clid);
        if (cl == null)
            fail("charlist widget vanished");
        // 1. The "add" uimsg must have decoded into a real Char entry
        //    (name + avatar layer resource ids). An empty list means the
        //    login screen would show a card with no portrait and no Play
        //    button - the reported "client frozen" symptom.
        List<?> chars = null;
        long deadline = System.currentTimeMillis() + 15000;
        while (System.currentTimeMillis() < deadline) {
            Object l = field(cl, "chars");
            if (l instanceof List && !((List<?>) l).isEmpty()) {
                chars = (List<?>) l;
                break;
            }
            Thread.sleep(100);
        }
        if (chars == null)
            fail("charlist add never decoded (no char entries: no portrait, "
                    + "no play button on the login screen)");
        Object ch = chars.get(0);
        Object nm = field(ch, "name");
        System.out.println("PROBE: char entry name=" + nm);

        // 2. The avatar layer resources must resolve client-side; this is
        //    the exact data the login-screen portrait (AvaRender) composites.
        Object ava = field(ch, "ava");
        if (ava == null)
            fail("char entry has no Avaview");
        Object myown = field(ava, "myown");
        if (myown == null)
            fail("Avaview has no local AvaRender (empty layer list?)");
        @SuppressWarnings("unchecked")
        List<Indir<Resource>> layers = (List<Indir<Resource>>) field(myown, "layers");
        if (layers.isEmpty())
            fail("avatar layer list is empty (server sent no layer resids)");
        int total = 0;
        List<String> unresolved = new ArrayList<>();
        deadline = System.currentTimeMillis() + 20000;
        while (true) {
            unresolved.clear();
            total = 0;
            for (Indir<Resource> r : layers) {
                Resource res = r.get();
                if (res == null)
                    unresolved.add(String.valueOf(r));
                else {
                    int n = res.layers(Resource.Image.class).size();
                    total += n;
                    System.out.println("PROBE: ava layer " + res.name
                            + " ver=" + res.ver + " imgs=" + n);
                }
            }
            if (unresolved.isEmpty())
                break;
            if (System.currentTimeMillis() >= deadline)
                fail("avatar layers never resolved: " + unresolved);
            Thread.sleep(100);
        }
        if (total == 0)
            fail("avatar layers resolved but contain zero image layers "
                    + "(portrait would be blank)");
        System.out.println("PROBE: avatar composite imgs=" + total);

        // 3. Enter the world through the REAL click chain: Button.click ->
        //    wdgmsg("activate") -> Charlist.wdgmsg -> wdgmsg("play").
        Object plb = field(ch, "plb");
        if (!(plb instanceof Button))
            fail("char entry has no Play button");
        ((Button) plb).click();
        System.out.println("PROBE: play clicked via the real button chain");
    }

    /** Wait for the post-play world widget tree (mapview + slen). */
    static void waitForWorld(UI ui, AtomicReference<Throwable> ruiDeath)
            throws Exception {
        long deadline = System.currentTimeMillis() + 20000;
        while (System.currentTimeMillis() < deadline) {
            Throwable d = ruiDeath.get();
            if (d != null) {
                d.printStackTrace(System.out);
                fail("remoteui thread died: " + d);
            }
            if (findWidget(ui, MapView.class) >= 0
                    && findWidget(ui, SlenHud.class) >= 0)
                return;
            Thread.sleep(100);
        }
        fail("mapview never appeared (black screen reproduced)");
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
