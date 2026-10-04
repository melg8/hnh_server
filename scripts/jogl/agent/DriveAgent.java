import java.awt.Robot;
import java.awt.event.InputEvent;
import java.lang.reflect.Field;
import java.util.List;
import java.util.ArrayList;

/**
 * DriveAgent: drives the real Haven client under Xvfb and reports world
 * state. Launched as a -javaagent alongside -Dhaven.autoplay=<name>.
 *
 * Phase 1: find the LoginScreen, fill user/pass via reflection, click the
 *          login IButton (autoplay selects the char on the charlist).
 * Phase 2: wait for the mapview (UI.instance.ln), grab the player gob id
 *          from mapview args, sample the player position from OCache,
 *          click the map with a real AWT Robot click, sample again,
 *          print verdict lines MOVEMENT: MOVED|STUCK.
 */
public class DriveAgent {
    static Object ui;
    static Field lnField;

    public static void premain(String args, java.lang.instrument.Instrumentation inst) {
        Thread t = new Thread(() -> run());
        t.setDaemon(true);
        t.start();
        System.out.println("AGENT: premain entered; cp="
                + System.getProperty("java.class.path", ""));
    }

    // ---------- generic reflection helpers ----------

    static Object getStatic(Class<?> cls, String name) throws Exception {
        Field f = cls.getDeclaredField(name);
        f.setAccessible(true);
        return f.get(null);
    }

    static Object get(Object obj, String name) throws Exception {
        Field f = obj.getClass().getDeclaredField(name);
        f.setAccessible(true);
        return f.get(obj);
    }

    static <T> List<T> collectChildren(Object w, Class<T> cls) throws Exception {
        List<T> out = new ArrayList<T>();
        Object child = w.getClass().getField("child").get(w);
        while (child != null) {
            if (cls.isInstance(child)) out.add(cls.cast(child));
            child = child.getClass().getField("next").get(child);
        }
        return out;
    }

    // ---------- phase 1: login ----------

    static boolean driveLogin(String user, String pass) {
        try {
            Object root = ui.getClass().getField("root").get(ui);
            Object loginScreen = null;
            for (Object w : collectChildren(root, Class.forName("haven.LoginScreen"))) {
                loginScreen = w;
            }
            if (loginScreen == null) return false;
            Object cur;
            try {
                cur = get(loginScreen, "cur");
            } catch (Exception e) {
                return false; // no login box yet (token screen or progress)
            }
            if (cur == null) return false;
            Object userEntry = get(cur, "user");
            Object passEntry = get(cur, "pass");
            if (userEntry == null || passEntry == null) return false;
            Class<?> te = userEntry.getClass();
            te.getField("text").set(userEntry, user);
            te.getField("text").set(passEntry, pass);
            Object btn = get(loginScreen, "btn");
            if (btn == null) return false;
            btn.getClass().getMethod("click").invoke(btn);
            System.out.println("AGENT: login submitted");
            return true;
        } catch (Throwable e) {
            return false;
        }
    }

    // ---------- phase 2: movement ----------

    static Object getLn() throws Exception {
        if (lnField == null) {
            lnField = ui.getClass().getDeclaredField("mapview");
            lnField.setAccessible(true);
        }
        return lnField.get(ui);
    }

    static Object getPlayerGob(Object mv) throws Exception {
        if (mv == null) return null;
        Field nF = mv.getClass().getDeclaredField("playergob");
        nF.setAccessible(true);
        int n = nF.getInt(mv);
        if (n < 0) return null;
        Object glob = get(mv, "glob");
        Object oc = get(glob, "oc");
        java.lang.reflect.Method getgob = oc.getClass().getMethod("getgob", int.class);
        return getgob.invoke(oc, n);
    }

    static int[] gobPos(Object gob) throws Exception {
        // Gob.position() returns the interpolated world coord while a
        // LIN move is active (Moving.getc()), else the static rc.
        java.lang.reflect.Method pos = gob.getClass().getMethod("position");
        Object c = pos.invoke(gob);
        if (c == null) return null;
        Field xF = c.getClass().getField("x");
        Field yF = c.getClass().getField("y");
        return new int[] { xF.getInt(c), yF.getInt(c) };
    }

    static void dumpTree(Object w, int depth) {
        try {
            if (depth > 4) return;
            StringBuilder sb = new StringBuilder();
            for (int i = 0; i < depth; i++) sb.append("  ");
            sb.append(w.getClass().getSimpleName()).append(" c=")
              .append(coord(w.getClass().getField("c").get(w)))
              .append(" sz=")
              .append(coord(w.getClass().getField("sz").get(w)));
            System.out.println("WIDGETS: " + sb);
            Object child = w.getClass().getField("child").get(w);
            while (child != null) {
                dumpTree(child, depth + 1);
                child = child.getClass().getField("next").get(child);
            }
        } catch (Throwable e) {
            System.out.println("WIDGETS: dump error " + e);
        }
    }

    static String coord(Object c) {
        try {
            if (c == null) return "null";
            Field xF = c.getClass().getField("x");
            Field yF = c.getClass().getField("y");
            return xF.getInt(c) + "," + yF.getInt(c);
        } catch (Throwable e) { return "?"; }
    }

    static void run() {
        String username = System.getProperty("haven.autoplay", "driveuser");
        try {
            long deadline = System.currentTimeMillis() + 120000;
            int hb = 0;
            Throwable lastErr = null;
            while (System.currentTimeMillis() < deadline) {
                try {
                    ui = getStatic(Class.forName("haven.UI"), "instance");
                } catch (Throwable e) {
                    lastErr = e;
                    if (e instanceof ClassNotFoundException) lastErr = null;
                }
                if (ui != null) break;
                if (++hb % 10 == 0) {
                    String diag = "";
                    try {
                        Class<?> uicls = Class.forName("haven.UI");
                        diag = " cls=" + uicls.getName() + " loader="
                                + uicls.getClassLoader() + " statics=";
                        Field[] fs = uicls.getDeclaredFields();
                        for (Field fl : fs) if (fl.getName().contains("inst")) diag += fl.getName() + " ";
                        Object mf = getStatic(Class.forName("haven.MainFrame"), "instance");
                        diag += " mainframe=" + (mf != null);
                    } catch (Throwable e2) {
                        diag = " diagerr=" + e2;
                    }
                    System.out.println("AGENT: waiting for UI instance (" + hb + ")"
                            + (lastErr != null ? " err=" + lastErr : "") + diag);
                }
                Thread.sleep(500);
            }
            if (ui == null) { System.out.println("AGENT: no UI instance"); return; }
            System.out.println("AGENT: UI up");

            // Phase 1: submit the login form when it appears.
            deadline = System.currentTimeMillis() + 60000;
            boolean logged = false;
            while (System.currentTimeMillis() < deadline) {
                if (driveLogin(username, "x")) { logged = true; break; }
                Thread.sleep(700);
            }
            System.out.println("AGENT: login " + (logged ? "sent" : "NOT sent"));

            // Phase 2: wait for the mapview.
            Object mv = null;
            deadline = System.currentTimeMillis() + 120000;
            int hb2 = 0;
            Throwable lastLnErr = null;
            while (System.currentTimeMillis() < deadline) {
                try {
                    // The client replaces the UI object when the session
                    // changes (login UI -> world UI); always re-read.
                    ui = getStatic(Class.forName("haven.UI"), "instance");
                    mv = getLn();
                    if (mv != null) break;
                } catch (Throwable e) {
                    lastLnErr = e;
                }
                if (++hb2 % 16 == 0)
                    System.out.println("AGENT: waiting for mapview (" + hb2 + ")"
                            + (lastLnErr != null ? " err=" + lastLnErr : ""));
                Thread.sleep(500);
            }
            if (mv == null) { System.out.println("AGENT: no mapview"); return; }
            System.out.println("AGENT: mapview up");

            Object pg = null;
            deadline = System.currentTimeMillis() + 60000;
            while (System.currentTimeMillis() < deadline) {
                pg = getPlayerGob(mv);
                if (pg != null) break;
                Thread.sleep(500);
            }
            if (pg == null) { System.out.println("AGENT: no player gob"); return; }
            int[] p0 = gobPos(pg);
            System.out.println("AGENT: player gob at " + (p0 == null ? "?" : p0[0] + "," + p0[1]));

            Thread.sleep(3000);
            p0 = gobPos(pg);
            System.out.println("AGENT: before click " + p0[0] + "," + p0[1]);

            Robot robot = new Robot();
            robot.setAutoDelay(40);
            robot.setAutoWaitForIdle(true);
            robot.mouseMove(400, 340);
            Thread.sleep(200);
            robot.mousePress(InputEvent.BUTTON1_DOWN_MASK);
            robot.mouseRelease(InputEvent.BUTTON1_DOWN_MASK);
            System.out.println("AGENT: clicked center");

            int[] last = p0;
            for (int i = 0; i < 20; i++) {
                Thread.sleep(300);
                Object g = getPlayerGob(mv);
                if (g == null) continue;
                int[] p = gobPos(g);
                if (p == null) continue;
                last = p;
                System.out.println("AGENT: pos " + p[0] + "," + p[1]);
            }
            boolean moved = (last[0] != p0[0]) || (last[1] != p0[1]);
            System.out.println("MOVEMENT: " + (moved ? "MOVED" : "STUCK") +
                    " from " + p0[0] + "," + p0[1] + " to " + last[0] + "," + last[1]);

            robot.mouseMove(520, 300);
            Thread.sleep(200);
            robot.mousePress(InputEvent.BUTTON1_DOWN_MASK);
            robot.mouseRelease(InputEvent.BUTTON1_DOWN_MASK);
            System.out.println("AGENT: clicked offset");
            int[] p1 = last;
            for (int i = 0; i < 20; i++) {
                Thread.sleep(300);
                Object g = getPlayerGob(mv);
                if (g == null) continue;
                int[] p = gobPos(g);
                if (p == null) continue;
                last = p;
            }
            boolean moved2 = (last[0] != p1[0]) || (last[1] != p1[1]);
            System.out.println("MOVEMENT2: " + (moved2 ? "MOVED" : "STUCK"));

            // Visual evidence: full-window screenshot with the avatar in
            // view (portrait verification) written outside the repo.
            try {
                java.awt.image.BufferedImage img = robot.createScreenCapture(
                        new java.awt.Rectangle(0, 0, 1024, 768));
                javax.imageio.ImageIO.write(img, "png",
                        new java.io.File("/tmp/client_world_" + System.currentTimeMillis() + ".png"));
                System.out.println("SCREENSHOT: saved");
            } catch (Throwable e) {
                System.out.println("SCREENSHOT: failed " + e);
            }
        } catch (Throwable e) {
            System.out.println("AGENT ERROR: " + e);
            e.printStackTrace();
        }
    }
}
