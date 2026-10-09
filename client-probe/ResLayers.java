import haven.Resource;

public class ResLayers {
    public static void main(String[] a) throws Exception {
        Resource.addurl(new java.net.URL("http://127.0.0.1:1872/"));
        Resource r = Resource.load(a[0], Integer.parseInt(a[1]));
        while (r.loading) Thread.sleep(20);
        System.out.println("res " + a[0] + " layers:");
        for (Class<Resource.Layer> t : new Class[]{Resource.Image.class,
                Resource.AButton.class, Resource.Code.class}) {
            System.out.println("  " + t.getSimpleName() + " = " + r.layer(t));
        }
        System.out.println("action=" + r.layer(Resource.action));
    }
}
