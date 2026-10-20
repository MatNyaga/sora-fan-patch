// Test helper for app_release_gui.sh: sends WM_DELETE_WINDOW (= the user clicking the window's close button) to the
// mapped, normal (not override-redirect, >200 px wide) window whose WM_CLASS contains argv[1]
// (WebKitGTK also maps small offscreen helper windows with the same class). Build: gcc -O1 -o wmclose wmclose.c -lX11
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <string.h>
static Window find(Display *d, Window w, const char *s) {
  XClassHint h; XWindowAttributes a;
  if (XGetClassHint(d, w, &h)) {
    int hit = (h.res_name && strstr(h.res_name, s)) || (h.res_class && strstr(h.res_class, s));
    if (h.res_name) XFree(h.res_name);
    if (h.res_class) XFree(h.res_class);
    if (hit && XGetWindowAttributes(d, w, &a) && a.map_state == IsViewable && !a.override_redirect && a.width > 200)
      return w;
  }
  Window r, p, *kids; unsigned n;
  if (!XQueryTree(d, w, &r, &p, &kids, &n)) return 0;
  Window f = 0;
  for (unsigned i = 0; i < n && !f; i++) f = find(d, kids[i], s);
  if (kids) XFree(kids);
  return f;
}
int main(int c, char **v) {
  if (c < 2) return 2;
  Display *d = XOpenDisplay(NULL); if (!d) return 2;
  Window w = find(d, DefaultRootWindow(d), v[1]); if (!w) { puts("no window"); return 1; }
  XEvent e; memset(&e, 0, sizeof e);
  e.xclient.type = ClientMessage; e.xclient.window = w;
  e.xclient.message_type = XInternAtom(d, "WM_PROTOCOLS", False); e.xclient.format = 32;
  e.xclient.data.l[0] = XInternAtom(d, "WM_DELETE_WINDOW", False); e.xclient.data.l[1] = CurrentTime;
  XSendEvent(d, w, False, NoEventMask, &e); XFlush(d); printf("close sent to 0x%lx\n", w); return 0;
}
