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

    /** Field lookup up the class hierarchy (TexI.back lives on the parent). */
    static Object getInherited(Object obj, String name) throws Exception {
        for (Class<?> c = obj.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                return f.get(obj);
            } catch (NoSuchFieldException ignore) {}
        }
        throw new NoSuchFieldException(name);
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
        String username = System.getProperty("haven.driveuser",
                System.getProperty("haven.autoplay", "driveuser"));
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

            // Phase 1.5: the selection screen is server-driven; the agent
            // waits for the character card (the "add" message populates
            // Charlist.chars), captures the portrait evidence, and then
            // picks the character itself through Charlist.choose_player.
            try {
                Object charlist = null;
                long clDeadline = System.currentTimeMillis() + 30000;
                while (System.currentTimeMillis() < clDeadline) {
                    try {
                        charlist = getStatic(Class.forName("haven.Charlist"), "instance");
                    } catch (Throwable ignore) {}
                    if (charlist != null) {
                        Object chars = get(charlist, "chars");
                        java.util.List<?> cl = (java.util.List<?>) chars;
                        if (cl != null && !cl.isEmpty()) break;
                    }
                    Thread.sleep(300);
                }
                if (charlist != null) {
                    // Let the layers load and the first frame render.
                    Thread.sleep(1500);
                    Object ch = ((java.util.List<?>) get(charlist, "chars")).get(0);
                    Object ava = get(ch, "ava");
                    Object myown = null;
                    try { myown = get(ava, "myown"); } catch (Throwable ignore) {}
                    if (myown != null) {
                        String d = (String) myown.getClass().getMethod("Dump").invoke(myown);
                        System.out.println("PORTRAIT LAYERS: "
                                + (d.isEmpty() ? "(none)" : d.replace('\n', '|')));
                        try {
                            Object imgs = getInherited(myown, "images");
                            int n = (imgs instanceof java.util.List) ? ((java.util.List<?>) imgs).size() : -1;
                            Object load = getInherited(myown, "loading");
                            System.out.println("AVATAR COMPOSITE: images=" + n + " loading=" + load);
                            if (imgs instanceof java.util.List) {
                                for (Object im : (java.util.List<?>) imgs) {
                                    Object off = getInherited(im, "o");
                                    Object imgf = getInherited(im, "img");
                                    System.out.println("LAYER: o=" + coord(off)
                                            + " img=" + (imgf != null ? imgf.getClass().getSimpleName() : "null"));
                                }
                            }
                            Object back = getInherited(myown, "back");
                            if (back instanceof java.awt.image.BufferedImage) {
                                javax.imageio.ImageIO.write(
                                        (java.awt.image.BufferedImage) back, "png",
                                        new java.io.File("/tmp/avatar_back.png"));
                                System.out.println("AVATAR BACK DUMP: saved");
                            }
                        } catch (Throwable e2) {
                            System.out.println("AVATAR COMPOSITE: err " + e2);
                        }
                    } else {
                        System.out.println("PORTRAIT LAYERS: (no AvaRender)");
                    }
                    Robot r0 = new Robot();
                    java.awt.image.BufferedImage img0 = r0.createScreenCapture(
                            new java.awt.Rectangle(0, 0, 1024, 768));
                    javax.imageio.ImageIO.write(img0, "png",
                            new java.io.File("/tmp/client_charlist.png"));
                    System.out.println("CHARLIST SCREENSHOT: saved");
                    Boolean picked = (Boolean) Class.forName("haven.Charlist")
                            .getMethod("choose_player", String.class)
                            .invoke(null, username);
                    System.out.println("AGENT: char pick " + (picked ? "sent" : "FAILED"));
                } else {
                    System.out.println("PORTRAIT: no charlist instance visible");
                }
            } catch (Throwable e) {
                System.out.println("PORTRAIT: err " + e);
            }

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

            // Sample the position until it rests for 3 consecutive samples
            // (or the sampling budget runs out); accumulate the path length
            // to measure the ACTUAL tiles-per-second the client renders.
            long t0 = System.currentTimeMillis();
            int[] prev = p0;
            int[] last = p0;
            double distTiles = 0;
            int stable = 0;
            int movedSamples = 0;
            boolean shotMid = false;
            int maxJump = 0;
            // DIAG: trace the client-side Moving attribute so a STUCK verdict
            // distinguishes "no LINBEG arrived" from "interpolation dead".
            for (int i = 0; i < 60 && stable < 3; i++) {
                Thread.sleep(250);
                Object g = getPlayerGob(mv);
                if (g == null) continue;
                int[] p = gobPos(g);
                if (p == null) continue;
                if (i < 6) {
                    try {
                        Object m = g.getClass().getMethod("getattr", Class.class)
                                .invoke(g, Class.forName("haven.Moving"));
                        Object aV = (m != null) ? get(m, "a") : "-";
                        Object cV = (m != null) ? get(m, "c") : "-";
                        Object rcV = get(g, "rc");
                        System.out.println("DIAG: i=" + i + " pos=" + p[0] + "," + p[1]
                                + " moving=" + (m != null) + " a=" + aV + " c=" + cV
                                + " rc=" + coord(rcV));
                    } catch (Throwable e) {
                        System.out.println("DIAG: err " + e);
                    }
                }
                int jump = Math.max(Math.abs(p[0] - prev[0]), Math.abs(p[1] - prev[1]));
                if (jump > maxJump) maxJump = jump;
                distTiles += Math.sqrt(Math.pow(p[0] - prev[0], 2) + Math.pow(p[1] - prev[1], 2)) / 11.0;
                boolean resting = (p[0] == prev[0] && p[1] == prev[1]);
                if (!resting) movedSamples++;
                prev = p;
                last = p;
                stable = resting ? stable + 1 : 0;
                if (i == 6 && !shotMid && (p[0] != p0[0] || p[1] != p0[1])) {
                    // Mid-walk visual evidence: walking pose must be visible.
                    java.awt.image.BufferedImage wimg = robot.createScreenCapture(
                            new java.awt.Rectangle(0, 0, 1024, 768));
                    javax.imageio.ImageIO.write(wimg, "png",
                            new java.io.File("/tmp/client_walking.png"));
                    System.out.println("WALKING SCREENSHOT: saved");
                    shotMid = true;
                }
            }
            // Speed is measured over MOVING samples only: the 3-sample
            // resting tail must not dilute the tiles-per-second estimate.
            double tps = movedSamples > 0 ? distTiles / (movedSamples * 0.25) : 0;
            boolean moved = (last[0] != p0[0]) || (last[1] != p0[1]);
            System.out.println("MOVEMENT: " + (moved ? "MOVED" : "STUCK") +
                    " from " + p0[0] + "," + p0[1] + " to " + last[0] + "," + last[1]);
            if (moved && movedSamples > 1) {
                System.out.println("SPEED: " + String.format("%.2f", tps)
                        + " tiles/s over " + String.format("%.1f", distTiles) + " tiles in "
                        + (movedSamples * 250) + " ms of motion");
                System.out.println("SPEED VERDICT: "
                        + ((tps > 2.0 && tps < 4.2) ? "OK" : "BAD"));
            }
            System.out.println("NO TELEPORT: " + (maxJump <= 33 ? "OK (max jump " + maxJump + " subtiles)" : "FAIL (jump " + maxJump + ")"));

            // Rapid re-clicks: five orders in quick succession, spread wide
            // enough that each one authorizes a real walk. Positions must
            // keep gliding between samples (no destination jump).
            int rapidMaxJump = 0;
            int[][] pts = {{380, 300}, {480, 380}, {360, 380}, {500, 320}, {420, 350}};
            int[] rprev = last;
            boolean rapidMoved = false;
            for (int[] pt : pts) {
                robot.mouseMove(pt[0], pt[1]);
                robot.mousePress(InputEvent.BUTTON1_DOWN_MASK);
                robot.mouseRelease(InputEvent.BUTTON1_DOWN_MASK);
                Thread.sleep(280);
                Object g = getPlayerGob(mv);
                if (g == null) continue;
                int[] p = gobPos(g);
                if (p == null) continue;
                int jump = Math.max(Math.abs(p[0] - rprev[0]), Math.abs(p[1] - rprev[1]));
                if (jump > rapidMaxJump) rapidMaxJump = jump;
                if (jump > 0) rapidMoved = true;
                rprev = p;
                last = p;
            }
            System.out.println("RAPID CLICKS: " + (rapidMaxJump <= 33 ? "GLIDING (max jump " + rapidMaxJump + ", moved " + rapidMoved + ")" : "TELEPORT (jump " + rapidMaxJump + ")"));
            boolean moved2 = rapidMoved;
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
