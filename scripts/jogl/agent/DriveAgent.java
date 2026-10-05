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
        collectChildrenRec(w, cls, out, 0);
        return out;
    }

    @SuppressWarnings("unchecked")
    static void collectChildrenRec(Object w, Class<?> cls, List<?> out, int depth) throws Exception {
        if (depth > 12) return;
        Object child = w.getClass().getField("child").get(w);
        while (child != null) {
            if (cls.isInstance(child)) ((List<Object>) out).add(child);
            collectChildrenRec(child, cls, out, depth + 1);
            child = child.getClass().getField("next").get(child);
        }
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

            // ---------- phase 3: session 21 evidence ----------
            // m2s(c) = (2x - 2y, x + y): +x (east) draws toward screen
            // (right, down). Clicking center + (220, 110) orders a pure
            // +x leg; center - (220, 110) a pure -x leg. Mid-walk frames
            // must show the walk pose FACING the travel direction.
            int[] center = {512, 384};
            // Screen deltas per 110 map subtiles: m2s(d) = (2dx-2dy, dx+dy).
            // EAST (+x) = (220, 110); NORTH (-y) = (220, -110);
            // SOUTH (+y) = (-220, 110). UP (away, (-1,-1)) = (0, -220);
            // LEFT ((-1,1)) = (-220, 0). UP and LEFT are the exact user-
            // reported defect directions (session 22): the walk pose must
            // face the travel direction (back view for UP, left profile
            // for LEFT).
            walkAndShoot(robot, mv, center, 220, 110, "EAST", "/tmp/client_walk_east.png");
            walkAndShoot(robot, mv, center, 220, -110, "NORTH", "/tmp/client_walk_north.png");
            walkAndShoot(robot, mv, center, -220, 110, "SOUTH", "/tmp/client_walk_south.png");
            walkAndShoot(robot, mv, center, 0, -220, "UP", "/tmp/client_walk_up.png");
            walkAndShoot(robot, mv, center, -220, 0, "LEFT", "/tmp/client_walk_left.png");

            // Equipment paperdoll: Equipory.cdraw draws ui.equip's bg +
            // the Avatar.rend of the avagob. Dump the binding state and
            // capture the window region (the missing-doll defect).
            try {
                Object equip = get(ui, "equip");
                if (equip != null) {
                    Object avagob = get(equip, "avagob");
                    String dump;
                    try {
                        Object sess = get(ui, "sess");
                        Object glob = get(sess, "glob");
                        Object oc = get(glob, "oc");
                        int avid = ((Number) avagob).intValue();
                        Object dgob = oc.getClass().getMethod("getgob", int.class)
                                .invoke(oc, avid);
                        if (dgob != null) {
                            Object ava = dgob.getClass().getMethod("getattr", Class.class)
                                    .invoke(dgob, Class.forName("haven.Avatar"));
                            if (ava != null) {
                                Object rend = get(ava, "rend");
                                dump = "ava-rend=" + (rend != null ? "OK" : "NULL");
                            } else {
                                dump = "no-Avatar-attr";
                            }
                        } else {
                            dump = "avagob-gob-missing";
                        }
                    } catch (Throwable e2) {
                        dump = "dump-err " + e2;
                    }
                    System.out.println("EQUIP DOLL: avagob=" + avagob + " " + dump);
                    Object wc = getInherited(equip, "c");
                    Object wsz = getInherited(equip, "sz");
                    int ex = Integer.parseInt(coord(wc).split(",")[0]);
                    int ey = Integer.parseInt(coord(wc).split(",")[1]);
                    int ew = Math.min(512, Integer.parseInt(coord(wsz).split(",")[0]));
                    int eh = Math.min(400, Integer.parseInt(coord(wsz).split(",")[1]));
                    java.awt.image.BufferedImage eq = robot.createScreenCapture(
                            new java.awt.Rectangle(ex, ey, ew, eh));
                    javax.imageio.ImageIO.write(eq, "png",
                            new java.io.File("/tmp/client_equip.png"));
                    System.out.println("EQUIP SCREENSHOT: saved at " + ex + "," + ey
                            + " " + ew + "x" + eh);
                } else {
                    System.out.println("EQUIP DOLL: no equipory window");
                }
            } catch (Throwable e) {
                System.out.println("EQUIP DOLL: err " + e);
            }

            // Animals in view: the 500-subtile visibility radius far exceeds
            // the rendered viewport and animal density is low, so passively
            // waiting rarely puts one on screen. Rounds: census -> if no
            // kritter inside the viewport, walk toward the nearest one and
            // re-census after arrival.
            try {
                boolean shot = false;
                for (int round = 0; round < 5 && !shot; round++) {
                    Object sess = get(ui, "sess");
                    Object glob = get(sess, "glob");
                    Object oc = get(glob, "oc");
                    // The fork camera pans with the mouse (edge-follow), so
                    // the viewport center is mv.mc + viewoffset(sz, mc) -
                    // read the LIVE camera instead of assuming the player is
                    // centered: screen = m2s(gob.pos - mc) + sz/2.
                    Object mcv = get(mv, "mc");
                    Object msz = getInherited(mv, "sz");
                    int vcx = Integer.parseInt(coord(msz).split(",")[0]) / 2;
                    int vcy = Integer.parseInt(coord(msz).split(",")[1]) / 2;
                    int mcx = Integer.parseInt(coord(mcv).split(",")[0]);
                    int mcy = Integer.parseInt(coord(mcv).split(",")[1]);
                    int bestD = Integer.MAX_VALUE;
                    int[] bestScreen = null;
                    String bestRes = "?";
                    Object it = oc.getClass().getMethod("iterator").invoke(oc);
                    while (it != null && ((java.util.Iterator<?>) it).hasNext()) {
                        Object g2 = ((java.util.Iterator<?>) it).next();
                        String[] names = (String[]) g2.getClass().getMethod("resnames").invoke(g2);
                        boolean kritter = false;
                        for (String n : names) {
                            // Prefer predators: passive species flee the
                            // player (their flee radius exceeds the
                            // viewport), while wolves and boars CHASE the
                            // player into view and stay within reach.
                            if (n != null && (n.contains("/wolf/") || n.contains("/boar/"))) {
                                kritter = true;
                                break;
                            }
                        }
                        if (!kritter) continue;
                        int[] gp = gobPos(g2);
                        if (gp == null) continue;
                        int rx = (gp[0] - mcx), ry = (gp[1] - mcy);
                        int sx = 2 * rx - 2 * ry + vcx;
                        int sy = rx + ry + vcy;
                        int d = Math.abs(sx - vcx) + Math.abs(sy - vcy);
                        if (d < bestD) {
                            bestD = d;
                            bestScreen = new int[] {sx, sy};
                            bestRes = names.length > 0 ? names[0] : "?";
                        }
                    }
                    if (bestScreen != null
                            && Math.abs(bestScreen[0] - vcx) < 420
                            && bestScreen[1] > 60 && bestScreen[1] < 540) {
                        java.awt.image.BufferedImage an = robot.createScreenCapture(
                                new java.awt.Rectangle(0, 0, 1024, 768));
                        javax.imageio.ImageIO.write(an, "png",
                                new java.io.File("/tmp/client_animals.png"));
                        System.out.println("ANIMALS SCREENSHOT: saved kritter at "
                                + bestScreen[0] + "," + bestScreen[1] + " res=" + bestRes);
                        shot = true;
                        break;
                    }
                    // Walk toward the nearest kritter: clamp its screen
                    // offset into the clickable viewport and click there.
                    if (bestScreen != null) {
                        int dx = bestScreen[0] - vcx;
                        int dy = bestScreen[1] - vcy;
                        double len = Math.max(1.0, Math.hypot(dx, dy));
                        double step = Math.min(1.0, 220.0 / len);
                        int cx = vcx + (int) Math.round(dx * step);
                        int cy = vcy + (int) Math.round(dy * step);
                        cx = Math.max(120, Math.min(900, cx));
                        cy = Math.max(80, Math.min(500, cy));
                        System.out.println("ANIMAL HUNT: nearest at screen "
                                + bestScreen[0] + "," + bestScreen[1] + "; clicking " + cx + "," + cy);
                        robot.mouseMove(cx, cy);
                        robot.mousePress(java.awt.event.InputEvent.BUTTON1_DOWN_MASK);
                        robot.mouseRelease(java.awt.event.InputEvent.BUTTON1_DOWN_MASK);
                        // Wait for the leg to finish (position rest or 25 s).
                        int[] huntLast = gobPos(getPlayerGob(mv));
                        long wdl = System.currentTimeMillis() + 25000;
                        int huntStable = 0;
                        while (System.currentTimeMillis() < wdl && huntStable < 4) {
                            Thread.sleep(400);
                            Object g = getPlayerGob(mv);
                            if (g == null) continue;
                            int[] p = gobPos(g);
                            if (p == null) continue;
                            if (p[0] == huntLast[0] && p[1] == huntLast[1]) huntStable++;
                            else huntStable = 0;
                            huntLast = p;
                        }
                        // Park the mouse centrally: the fork camera pans
                        // toward the mouse when it rests near the view
                        // border, which would keep drifting the frame.
                        robot.mouseMove(512, 340);
                        Thread.sleep(1200);
                    } else {
                        Thread.sleep(2000);
                    }
                }
                if (!shot) {
                    robot.mouseMove(512, 340);
                    Thread.sleep(1500);
                    java.awt.image.BufferedImage an = robot.createScreenCapture(
                            new java.awt.Rectangle(0, 0, 1024, 768));
                    javax.imageio.ImageIO.write(an, "png",
                            new java.io.File("/tmp/client_animals.png"));
                    System.out.println("ANIMALS SCREENSHOT: saved fallback (no predator in viewport)");
                }
            } catch (Throwable e) {
                System.out.println("ANIMALS SCREENSHOT: err " + e);
            }

            // ---------- phase 4: session 25 equipment visuals ----------
            // Equip a starter clothing piece through the same widget
            // wdgmsg path the mouse uses (Item "take" onto the cursor,
            // epry "drop" into a slot), then read the Avatar.rend dump:
            // the doll recomposites from the streamed OD_AVATAR, so the
            // dump naming the piece's borka layers is the wire-level
            // proof, and the screenshots are the visual one.
            try {
                Object equip4 = get(ui, "equip");
                if (equip4 == null) {
                    System.out.println("EQUIPVIS: no equipory window");
                } else {
                    Class<?> itemCls = Class.forName("haven.Item");
                    Object root4 = ui.getClass().getField("root").get(ui);
                    // Open the inventory first: the item widgets are
                    // created server-side only when the window exists
                    // (the same wdgmsg the HUD's inventory button sends).
                    Object slen = get(ui, "slenhud");
                    if (slen != null) {
                        java.lang.reflect.Method sm = slen.getClass()
                                .getMethod("wdgmsg", String.class, Object[].class);
                        sm.invoke(slen, new Object[]{"inv", new Object[]{}});
                        Thread.sleep(1200);
                    }
                    @SuppressWarnings("rawtypes")
                    java.util.List items = collectChildren(root4, itemCls);
                    // The starter kit ships branch/stone/meat/seeds AND
                    // linen pants + shirt: pick the linen pants by its
                    // RESOURCE name (Item.name() is the display tooltip;
                    // GetResName() is the wire resource path).
                    Object bagItem = null;
                    String bagName = "?";
                    java.lang.reflect.Method resNameM = itemCls.getMethod("GetResName");
                    for (Object it : items) {
                        Field drF = itemCls.getField("isDragging");
                        if (drF.getBoolean(it)) continue;
                        String nm = String.valueOf(resNameM.invoke(it));
                        System.out.println("EQUIPVIS: inventory item " + nm);
                        if (nm != null && nm.contains("linenpants")) {
                            bagItem = it;
                            bagName = nm;
                            break;
                        }
                    }
                    if (bagItem == null) {
                        System.out.println("EQUIPVIS: no linen pants in the inventory");
                    } else {
                    System.out.println("EQUIPVIS: equipping " + bagName);
                    java.lang.reflect.Method wdgmsgM = itemCls
                            .getMethod("wdgmsg", String.class, Object[].class);
                    Object coordZ = Class.forName("haven.Coord").getField("z").get(null);
                    // Take the wearable onto the cursor, drop it into
                    // slot 2 (legs) through the epry widget itself.
                    wdgmsgM.invoke(bagItem, new Object[]{"take", new Object[]{coordZ}});
                    Thread.sleep(600);
                    java.lang.reflect.Method eqmsg = equip4.getClass()
                            .getMethod("wdgmsg", String.class, Object[].class);
                    eqmsg.invoke(equip4, new Object[]{"drop", new Object[]{Integer.valueOf(2)}});
                    // Resources for the piece's borka layers stream in on
                    // demand; give the doll time to load and recomposite.
                    Thread.sleep(3500);
                    String dressed = avaDump(mv);
                    System.out.println("EQUIPVIS DUMP DRESSED: " + dressed.replace('\n', '|'));
                    // Large doll-region screenshots for the visual diff:
                    // the Equipment window sits at its laid-out position.
                    try {
                        Object wc4 = getInherited(equip4, "c");
                        Object wsz4 = getInherited(equip4, "sz");
                        int ex4 = Integer.parseInt(coord(wc4).split(",")[0]);
                        int ey4 = Integer.parseInt(coord(wc4).split(",")[1]);
                        int ew4 = Math.min(512, Integer.parseInt(coord(wsz4).split(",")[0]));
                        int eh4 = Math.min(400, Integer.parseInt(coord(wsz4).split(",")[1]));
                        java.awt.image.BufferedImage ed = robot.createScreenCapture(
                                new java.awt.Rectangle(ex4, ey4, ew4, eh4));
                        javax.imageio.ImageIO.write(ed, "png", new java.io.File("/tmp/client_equip_dressed.png"));
                        System.out.println("EQUIPVIS DOLL SHOT: dressed saved " + ex4 + "," + ey4 + " " + ew4 + "x" + eh4);
                    } catch (Throwable e3) {
                        System.out.println("EQUIPVIS DOLL SHOT: err " + e3);
                    }
                    // Park the mouse at the screen center so the fork
                    // camera pans back to the player, then capture the
                    // world avatar in clothes: crop around the gob's
                    // projected screen position (m2s = (2x-2y, x+y)).
                    robot.mouseMove(512, 384);
                    Thread.sleep(1800);
                    java.awt.image.BufferedImage d1 = robot.createScreenCapture(
                            new java.awt.Rectangle(0, 0, 1024, 768));
                    javax.imageio.ImageIO.write(d1, "png", new java.io.File("/tmp/client_world_dressed.png"));
                    System.out.println("WORLD DUMP DRESSED: " + worldLayerDump(mv));
                    try {
                        Object g2 = getPlayerGob(mv);
                        int[] gp = gobPos(g2);
                        Object mcv2 = get(mv, "mc");
                        Object msz2 = getInherited(mv, "sz");
                        int vcx2 = Integer.parseInt(coord(msz2).split(",")[0]) / 2;
                        int vcy2 = Integer.parseInt(coord(msz2).split(",")[1]) / 2;
                        int mcx2 = Integer.parseInt(coord(mcv2).split(",")[0]);
                        int mcy2 = Integer.parseInt(coord(mcv2).split(",")[1]);
                        int sx = (2 * (gp[0] - mcx2) - 2 * (gp[1] - mcy2)) + vcx2;
                        int sy = ((gp[0] - mcx2) + (gp[1] - mcy2)) + vcy2;
                        sx = Math.max(60, Math.min(960, sx));
                        sy = Math.max(60, Math.min(700, sy));
                        javax.imageio.ImageIO.write(d1.getSubimage(sx - 45, sy - 60, 90, 110),
                                "png", new java.io.File("/tmp/client_world_player.png"));
                        System.out.println("WORLD PLAYER SHOT: saved at " + sx + "," + sy);
                    } catch (Throwable e4) {
                        System.out.println("WORLD PLAYER SHOT: err " + e4);
                    }
                    // Unequip: the doll must drop the piece again.
                    eqmsg.invoke(equip4, new Object[]{"take", new Object[]{Integer.valueOf(2), Integer.valueOf(0)}});
                    Thread.sleep(3500);
                    String undressed = avaDump(mv);
                    System.out.println("EQUIPVIS DUMP UNDRESSED: " + undressed.replace('\n', '|'));
                    System.out.println("WORLD DUMP UNDRESSED: " + worldLayerDump(mv));
                    try {
                        Object wc5 = getInherited(equip4, "c");
                        Object wsz5 = getInherited(equip4, "sz");
                        int ex5 = Integer.parseInt(coord(wc5).split(",")[0]);
                        int ey5 = Integer.parseInt(coord(wc5).split(",")[1]);
                        int ew5 = Math.min(512, Integer.parseInt(coord(wsz5).split(",")[0]));
                        int eh5 = Math.min(400, Integer.parseInt(coord(wsz5).split(",")[1]));
                        java.awt.image.BufferedImage eu = robot.createScreenCapture(
                                new java.awt.Rectangle(ex5, ey5, ew5, eh5));
                        javax.imageio.ImageIO.write(eu, "png", new java.io.File("/tmp/client_equip_undressed.png"));
                        System.out.println("EQUIPVIS DOLL SHOT: undressed saved");
                    } catch (Throwable e3) {
                        System.out.println("EQUIPVIS DOLL SHOT: err " + e3);
                    }
                    String piece = "pants-linen";
                    boolean ok = dressed.contains(piece) && !undressed.contains(piece);
                    System.out.println("EQUIPVIS VERDICT: " + (ok ? "OK (doll recomposites with the equipped piece)" : "FAIL"));
                    // Re-equip for the closing screenshot set: the world
                    // avatar in clothes (the next screenshots carry it).
                    eqmsg.invoke(equip4, new Object[]{"drop", new Object[]{Integer.valueOf(2)}});
                    Thread.sleep(3000);
                    }
                }
            } catch (Throwable e) {
                System.out.println("EQUIPVIS: err " + e);
            }
        } catch (Throwable e) {
            System.out.println("AGENT ERROR: " + e);
            e.printStackTrace();
        }
    }

    /** The Avatar.rend layer dump of the viewer's own gob ("" when the
     *  attribute or the render is missing). */
    static String avaDump(Object mv) {
        try {
            Object g = getPlayerGob(mv);
            if (g == null) return "no-gob";
            Object ava = g.getClass().getMethod("getattr", Class.class)
                    .invoke(g, Class.forName("haven.Avatar"));
            if (ava == null) return "no-avatar-attr";
            Object rend = ava.getClass().getField("rend").get(ava);
            if (rend == null) return "no-rend";
            return (String) rend.getClass().getMethod("Dump").invoke(rend);
        } catch (Throwable e) {
            return "dump-err " + e;
        }
    }

    /** The world drawable's layer names (Layered.layers) of the player
     *  gob - the OD_LAYERS wire state the world renderer composites. */
    static String worldLayerDump(Object mv) {
        try {
            Object g = getPlayerGob(mv);
            if (g == null) return "no-gob";
            Object draw = g.getClass().getMethod("getattr", Class.class)
                    .invoke(g, Class.forName("haven.Drawable"));
            if (draw == null) return "no-drawable";
            if (!Class.forName("haven.Layered").isInstance(draw)) return "not-layered";
            Object layers = draw.getClass().getField("layers").get(draw);
            StringBuilder sb = new StringBuilder();
            for (Object r : (java.util.List<?>) layers) {
                java.lang.reflect.Method gm = r.getClass().getMethod("get");
                gm.setAccessible(true);
                Object res = gm.invoke(r);
                java.lang.reflect.Field nf = res.getClass().getField("name");
                nf.setAccessible(true);
                sb.append(nf.get(res)).append('|');
            }
            return sb.toString();
        } catch (Throwable e) {
            return "dump-err " + e;
        }
    }

    /** Click a map leg whose screen offset matches (dx, dy) per 110
     *  subtiles and capture a mid-walk frame. Retries with doubled
     *  distances; a click may land on an obstacle or a gob (interact
     *  instead of walk), so multiple attempts keep the evidence coming. */
    static void walkAndShoot(Robot robot, Object mv, int[] center, int dx, int dy,
                             String label, String shotPath) {
        int[] dists = {1, 2, 3};
        for (int attempt = 0; attempt < dists.length; attempt++) {
            try {
                Object g0 = getPlayerGob(mv);
                if (g0 == null) { System.out.println("WALKDIR: no gob"); return; }
                int[] before = gobPos(g0);
                int mult = dists[attempt];
                int sx = center[0] + dx * mult;
                int sy = center[1] + dy * mult;
                robot.mouseMove(sx, sy);
                robot.mousePress(java.awt.event.InputEvent.BUTTON1_DOWN_MASK);
                robot.mouseRelease(java.awt.event.InputEvent.BUTTON1_DOWN_MASK);
                boolean shooting = false;
                boolean shot = false;
                boolean arrived = false;
                int[] last = before;
                long dl = System.currentTimeMillis() + 15000;
                long start = System.currentTimeMillis();
                while (System.currentTimeMillis() < dl) {
                    Thread.sleep(250);
                    Object g = getPlayerGob(mv);
                    if (g == null) continue;
                    int[] p = gobPos(g);
                    if (p == null) continue;
                    if (!shooting && (p[0] != before[0] || p[1] != before[1])
                            && System.currentTimeMillis() - start > 700) {
                        shooting = true;
                    }
                    if (shooting && !shot) {
                        java.awt.image.BufferedImage wimg = robot.createScreenCapture(
                                new java.awt.Rectangle(0, 0, 1024, 768));
                        javax.imageio.ImageIO.write(wimg, "png", new java.io.File(shotPath));
                        System.out.println("WALKDIR SCREENSHOT: saved " + shotPath);
                        shot = true;
                    }
                    boolean resting = (p[0] == last[0] && p[1] == last[1]);
                    last = p;
                    if (resting && shot) { arrived = true; break; }
                }
                System.out.println("WALKDIR " + label + " attempt " + attempt + ": "
                        + (arrived ? "ARRIVED at " + last[0] + "," + last[1] : "no-arrival")
                        + " from " + before[0] + "," + before[1]);
                if (shot) return;
            } catch (Throwable e) {
                System.out.println("WALKDIR: err " + e);
                return;
            }
        }
        System.out.println("WALKDIR " + label + ": never captured (obstacles?)");
    }
}
