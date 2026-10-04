/*
 *  This file is part of the Haven & Hearth game client.
 *  Copyright (C) 2009 Fredrik Tolf <fredrik@dolda2000.com>, and
 *                     Björn Johannessen <johannessen.bjorn@gmail.com>
 *
 *  Redistribution and/or modification of this file is subject to the
 *  terms of the GNU Lesser General Public License, version 3, as
 *  published by the Free Software Foundation.
 *
 *  This program is distributed in the hope that it will be useful,
 *  but WITHOUT ANY WARRANTY; without even the implied warranty of
 *  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *  GNU General Public License for more details.
 *
 *  Other parts of this source tree adhere to other copying
 *  rights. Please see the file `COPYING' in the root directory of this
 *  source tree for details.
 *
 *  A copy the GNU Lesser General Public License is distributed along
 *  with the source tree of which this file is a part in the file
 *  `doc/LPGL-3'. If it is missing for any reason, please see the Free
 *  Software Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA
 *  02111-1307 USA
 */

package haven;

import static haven.Resource.imgc;
import java.awt.image.BufferedImage;
import java.util.*;

/**
 * Composites a player avatar from concrete image layers into one texture.
 *
 * This fork renders the composite CPU-side (TexI) instead of the original
 * TexRT screen-copy path: TexRT draws the layers onto the framebuffer under
 * a bottom-up ortho projection and copies the screen region back, which on
 * this render stack left the character-selection card empty (the
 * "no face at login" defect, reproduced and verified on the real client).
 * The layer list itself is server-resolved (the server sends concrete
 * standing/walking frame resources).
 */
public class AvaRender extends TexI {
    List<Indir<Resource>> layers;
    List<Resource.Image> images;
    boolean loading;
    public static final Coord sz = new Coord(212, 249);

    public AvaRender(List<Indir<Resource>> layers) {
        super(sz);
        setlay(layers);
        recomp();
    }

    public boolean hasImage(String mask) {
        for (Indir<Resource> r : layers) {
            if (r.get() != null) {
                if (r.get().name != null)
                    if (r.get().name.indexOf(mask) >= 0) {
                        return true;
                    }
            }
        }
        return false;
    }

    public String Dump() {
        StringBuilder sb = new StringBuilder();
        for (Indir<Resource> r : layers) {
            if (r.get() != null) {
                if (r.get().name != null) {
                    sb.append(r.get().name);
                    sb.append('\n');
                }
            }
        }
        return sb.toString();
    }

    public void setlay(List<Indir<Resource>> layers) {
        Collections.sort(layers);
        this.layers = layers;
        loading = true;
        recomp();
    }

    /**
     * Composite every loaded image layer into the backing buffer,
     * z-ordered. Keeps loading=true until all layers have arrived so the
     * composite refreshes as resources stream in.
     */
    private void recomp() {
        List<Resource.Image> imgs = new ArrayList<Resource.Image>();
        boolean pending = false;
        for (Indir<Resource> r : layers) {
            if (r.get() == null)
                pending = true;
            else
                imgs.addAll(r.get().layers(imgc));
        }
        Collections.sort(imgs);
        if (!pending && images != null && images.equals(imgs))
            return;
        images = imgs;
        loading = pending;
        BufferedImage buf = mkbuf(sz);
        java.awt.Graphics2D g = buf.createGraphics();
        // The world renderer places each layer at (center + img.o); the raw
        // offsets therefore cluster near the origin (the whole figure spans
        // roughly 27..59 x, 12..60 y). Anchor the figure's bounding-box
        // center into the rectangle Avaview actually shows: its draw offset
        // (tsz.x/2 - asz.x/2, yo).inv() over the 74x74 card exposes buffer
        // rect (69..143, 20..94), so center the figure at (sz.x/2, 57).
        int minx = Integer.MAX_VALUE, miny = Integer.MAX_VALUE;
        int maxx = Integer.MIN_VALUE, maxy = Integer.MIN_VALUE;
        boolean any = false;
        for (Resource.Image i : imgs) {
            if (i.img == null)
                continue;
            any = true;
            minx = Math.min(minx, i.o.x);
            miny = Math.min(miny, i.o.y);
            maxx = Math.max(maxx, i.o.x + i.sz.x);
            maxy = Math.max(maxy, i.o.y + i.sz.y);
        }
        int dx = 0, dy = 0;
        if (any) {
            dx = (sz.x / 2) - ((minx + maxx) / 2);
            dy = (57) - ((miny + maxy) / 2);
        }
        for (Resource.Image i : imgs) {
            if (i.img == null)
                continue;
            g.drawImage(i.img, dx + i.o.x, dy + i.o.y, null);
        }
        g.dispose();
        back = buf;
        update(convert(buf, tdim));
    }

    @Override
    public void render(GOut g, Coord c, Coord ul, Coord br, Coord sz) {
        // Refresh the composite while resources are still streaming in;
        // a no-op once every layer has arrived (loading == false).
        if (loading)
            recomp();
        super.render(g, c, ul, br, sz);
    }
}
