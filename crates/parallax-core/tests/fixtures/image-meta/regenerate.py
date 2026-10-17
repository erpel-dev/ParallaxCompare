#!/usr/bin/env python3
"""Rebuilds the image metadata fixtures. Needs Pillow (with AVIF and WebP) and piexif.

    python3 -I regenerate.py   (run from this folder)
"""
import io
import struct
import zlib

import piexif
from PIL import Image, ImageCms, ImageDraw
from PIL.PngImagePlugin import PngInfo


def picture(w=64, h=48):
    img = Image.new("RGB", (w, h), (40, 90, 160))
    d = ImageDraw.Draw(img)
    d.rectangle((8, 8, 30, 30), fill=(230, 200, 40))
    d.ellipse((34, 10, 58, 40), fill=(200, 40, 60))
    return img


def gps(deg):
    d = abs(deg)
    m = (d - int(d)) * 60
    s = (m - int(m)) * 60
    return ((int(d), 1), (int(m), 1), (int(round(s * 100)), 100))


def exif(date="2024:05:17 14:03:22", lat=48.858222, lon=2.2945, orientation=1):
    zeroth = {
        piexif.ImageIFD.Make: b"Canon",
        piexif.ImageIFD.Model: b"Canon EOS R6",
        piexif.ImageIFD.Orientation: orientation,
        piexif.ImageIFD.XResolution: (300, 1),
        piexif.ImageIFD.YResolution: (300, 1),
        piexif.ImageIFD.ResolutionUnit: 2,
        piexif.ImageIFD.Software: b"Parallax fixture 1.0",
        piexif.ImageIFD.Artist: b"Jane Doe",
        piexif.ImageIFD.Copyright: b"(c) 2024 Jane Doe",
    }
    ex = {
        piexif.ExifIFD.DateTimeOriginal: date.encode(),
        piexif.ExifIFD.DateTimeDigitized: date.encode(),
        piexif.ExifIFD.ExposureTime: (1, 250),
        piexif.ExifIFD.FNumber: (28, 10),
        piexif.ExifIFD.ISOSpeedRatings: 400,
        piexif.ExifIFD.FocalLength: (50, 1),
        piexif.ExifIFD.LensModel: b"RF50mm F1.8 STM",
        piexif.ExifIFD.Flash: 16,
        piexif.ExifIFD.ExposureProgram: 3,
        piexif.ExifIFD.MeteringMode: 5,
        piexif.ExifIFD.WhiteBalance: 0,
        piexif.ExifIFD.ColorSpace: 1,
        piexif.ExifIFD.MakerNote: bytes(range(256)) * 2,
    }
    g = {
        piexif.GPSIFD.GPSLatitudeRef: b"N" if lat >= 0 else b"S",
        piexif.GPSIFD.GPSLatitude: gps(lat),
        piexif.GPSIFD.GPSLongitudeRef: b"E" if lon >= 0 else b"W",
        piexif.GPSIFD.GPSLongitude: gps(lon),
        piexif.GPSIFD.GPSAltitudeRef: 0,
        piexif.GPSIFD.GPSAltitude: (35, 1),
    }
    return piexif.dump({"0th": zeroth, "Exif": ex, "GPS": g, "1st": {}, "thumbnail": None})


XMP = b"""<?xpacket begin="\xef\xbb\xbf" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" xmp:Rating="4" xmp:CreatorTool="Parallax fixture 1.0"
    photoshop:City="Paris">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Eiffel tower at noon</rdf:li></rdf:Alt></dc:title>
   <dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li></rdf:Seq></dc:creator>
   <dc:subject><rdf:Bag><rdf:li>paris</rdf:li><rdf:li>tower</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"""


def iptc_app13():
    def ds(num, value):
        v = value.encode()
        return struct.pack(">BBBH", 0x1C, 2, num, len(v)) + v
    iim = ds(5, "Eiffel tower") + ds(25, "paris") + ds(25, "tower") + ds(80, "Jane Doe") + ds(90, "Paris")
    if len(iim) % 2:
        iim += b"\0"
    res = b"8BIM" + struct.pack(">H", 0x0404) + b"\0\0" + struct.pack(">I", len(iim)) + iim
    payload = b"Photoshop 3.0\0" + res
    return b"\xff\xed" + struct.pack(">H", len(payload) + 2) + payload


ICC = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()


def jpeg(name, *, iptc=True, **kw):
    buf = io.BytesIO()
    picture().save(buf, "JPEG", quality=80, exif=exif(**kw), icc_profile=ICC, xmp=XMP)
    data = buf.getvalue()
    if iptc:  # after SOI and APP0
        app0_end = 2 + 2 + struct.unpack(">H", data[4:6])[0]
        data = data[:app0_end] + iptc_app13() + data[app0_end:]
    open(name, "wb").write(data)


jpeg("photo-v1.jpg")
jpeg("photo-v2.jpg", date="2024:05:18 09:41:07", lat=48.8606, lon=2.3376)
jpeg("photo-rotated.jpg", orientation=6)
picture().save("photo-stripped.jpg", "JPEG", quality=80)

info = PngInfo()
info.add_text("Title", "Test chart")
info.add_text("Author", "Jane Doe")
info.add_text("Description", "A line\nand a second line", zip=True)
info.add_itxt("Comment", "Grüße aus Köln", lang="de", tkey="Kommentar")
info.add_itxt("XML:com.adobe.xmp", XMP.decode())
info.add(b"gAMA", struct.pack(">I", 45455))
info.add(b"tIME", struct.pack(">HBBBBB", 2024, 5, 17, 14, 3, 22))
picture().save("image.png", "PNG", pnginfo=info, exif=exif(), icc_profile=ICC, dpi=(150, 150))
info2 = PngInfo()
info2.add_text("Title", "Test chart, revised")
info2.add_text("Author", "Jane Doe")
picture().save("image-v2.png", "PNG", pnginfo=info2, dpi=(150, 150))
Image.new("LA", (16, 8), (128, 200)).save("gray-alpha.png", "PNG")

picture().save("image.tif", "TIFF", exif=exif(), icc_profile=ICC, compression="tiff_lzw",
               tiffinfo={700: XMP, 271: "Nikon", 272: "Nikon Z 6", 306: "2024:05:17 14:03:22", 315: "Jane Doe"})
picture().save("image.webp", "WEBP", exif=exif(), icc_profile=ICC, xmp=XMP, quality=70)
picture().save("image.avif", "AVIF", exif=exif(), icc_profile=ICC, xmp=XMP, quality=50)
