import haven.Coord;
import haven.Resource;

import javax.imageio.ImageIO;
import java.awt.Graphics2D;
import java.awt.image.BufferedImage;
import java.io.File;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/** Offline replica of AvaRender.recomp: load the body doll layers +
 * pants from the disk resource source and composite them the same way.
 * Reveals whether the pants image lands on the doll at all. Run from a
 * CWD where ./res holds the resource tree (see scripts/run_ava_probe.sh). */
public class AvaProbe {
    public static void main(String[] args) throws Exception {
        String[] names = {
                "gfx/borka/body/standing/legs-0",
                "gfx/borka/body/standing/torso/male-0",
                "gfx/borka/body/standing/head-0",
                "gfx/borka/body/standing/arm/banzai/left-0",
                "gfx/borka/body/standing/arm/banzai/right-0",
                "gfx/borka/hair-karin/standing/hair-0",
                "gfx/borka/pants-linen/standing/pants-0",
        };
        List<Resource.Image> imgs = new ArrayList<Resource.Image>();
        for (String n : names) {
            Resource r = Resource.fromFile(n);
            r.loadwait();
            imgs.addAll(r.layers(Resource.imgc));
            System.out.println("loaded " + n + " images=" + r.layers(Resource.imgc).size());
        }
        Collections.sort(imgs);
        int minx = Integer.MAX_VALUE, miny = Integer.MAX_VALUE, maxx = Integer.MIN_VALUE, maxy = Integer.MIN_VALUE;
        for (Resource.Image i : imgs) {
            if (i.img == null) continue;
            minx = Math.min(minx, i.o.x);
            miny = Math.min(miny, i.o.y);
            maxx = Math.max(maxx, i.o.x + i.sz.x);
            maxy = Math.max(maxy, i.o.y + i.sz.y);
            System.out.println("img off=" + i.o + " sz=" + i.sz + " z=" + i.z);
        }
        int dx = (212 / 2) - ((minx + maxx) / 2);
        int dy = 57 - ((miny + maxy) / 2);
        BufferedImage buf = new BufferedImage(212, 249, BufferedImage.TYPE_INT_ARGB);
        Graphics2D g = buf.createGraphics();
        for (Resource.Image i : imgs) {
            g.drawImage(i.img, dx + i.o.x, dy + i.o.y, null);
        }
        g.dispose();
        ImageIO.write(buf, "png", new File("/tmp/ava_probe.png"));
        System.out.println("PROBE saved /tmp/ava_probe.png");
    }
}
