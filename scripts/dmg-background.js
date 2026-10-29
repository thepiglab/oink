// Draws the disk image window's background: an arrow from Oink to the Applications folder and a
// first-launch hint. Finder draws a window with a background picture in light mode whatever the
// system appearance, so this is designed for light mode only.
//
//   osascript -l JavaScript scripts/dmg-background.js OUT.png SCALE
//
// The image is 600 x 380 points; scripts/dmg-settings.py places the icons to match.
ObjC.import('AppKit');

function run(argv) {
  const [out, scale] = [argv[0], Number(argv[1])];
  const [W, H] = [600, 380];
  const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(
    null, W * scale, H * scale, 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0);
  rep.setSize($.NSMakeSize(W, H)); // draw in points; also records the DPI Finder needs for Retina
  $.NSGraphicsContext.saveGraphicsState;
  $.NSGraphicsContext.setCurrentContext($.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep));

  // AppKit's y axis points up; positions below are measured from the top, like Finder's.
  const rgb = (r, g, b, a = 1) => $.NSColor.colorWithSRGBRedGreenBlueAlpha(r / 255, g / 255, b / 255, a);
  const pt = (x, y) => $.NSMakePoint(x, H - y);

  $.NSGradient.alloc.initWithStartingColorEndingColor(rgb(255, 255, 255), rgb(252, 243, 246))
    .drawInRectAngle($.NSMakeRect(0, 0, W, H), -90);

  // Arrow between the icons (centres at x = 150 and 450, y = 170).
  const arrow = $.NSBezierPath.bezierPath;
  arrow.moveToPoint(pt(250, 170));
  arrow.lineToPoint(pt(350, 170));
  arrow.moveToPoint(pt(330, 152));
  arrow.lineToPoint(pt(350, 170));
  arrow.lineToPoint(pt(330, 188));
  arrow.setLineWidth(5);
  arrow.setLineCapStyle($.NSLineCapStyleRound);
  arrow.setLineJoinStyle($.NSLineJoinStyleRound);
  rgb(236, 150, 176).set;
  arrow.stroke;

  // Centred lines of 12 pt text; `y` is the baseline.
  const line = (text, y, weight, color) => {
    const attrs = $.NSDictionary.dictionaryWithObjectsForKeys(
      $([$.NSFont.systemFontOfSizeWeight(12, weight), color]),
      $([$.NSFontAttributeName, $.NSForegroundColorAttributeName]));
    const str = $(text);
    str.drawAtPointWithAttributes(pt((W - str.sizeWithAttributes(attrs).width) / 2, y), attrs);
  };
  line('Blocked on first launch?', 322, $.NSFontWeightSemibold, rgb(110, 110, 118));
  line('System Settings \u2192 Privacy & Security \u2192 Open Anyway', 340, $.NSFontWeightRegular, rgb(140, 140, 148));

  $.NSGraphicsContext.restoreGraphicsState;
  const png = rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $({}));
  if (!png.writeToFileAtomically(out, true)) throw new Error('could not write ' + out);
}
