"""Independent image identity and physical-feedback requirements."""
from pathlib import Path
import tempfile
import unittest
from PIL import Image,ImageDraw
from measure_desktop_latency import barcode,physical_feedback


class DesktopLatencyTests(unittest.TestCase):
    def image(self,word):
        image=Image.new('RGB',(1920,64));draw=ImageDraw.Draw(image)
        for bit in range(32):
            value=230 if word&(1<<bit) else 20
            draw.rectangle((bit*60,0,bit*60+59,63),fill=(value,)*3)
        draw.rectangle((0,0,1919,15),fill=(10,40,70))
        return image

    def test_independent_capture_requires_fixture_magic_and_exact_generation(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'image.png'
            for value in [0,1,40,65535]:
                self.image(0xa5c30000|value).save(path)
                self.assertEqual(barcode(path),value)
            self.image(0xffffffff).save(path);self.assertIsNone(barcode(path))

    def test_colour_or_mid_tone_corruption_cannot_identify_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'image.png'
            for pixel in [(128,128,128),(0,230,0)]:
                image=self.image(0xa5c30028);image.putpixel((30,32),pixel);image.save(path)
                self.assertIsNone(barcode(path))

    def test_headless_or_partial_hardware_flags_are_rejected(self):
        joined=dict(samples=[dict(viewer_update=1)])
        for flags in [0,1,2,4]:
            with self.subTest(flags=flags),self.assertRaises(ValueError):
                physical_feedback(joined,[dict(event='frame',frame=1,flags=flags)])
        self.assertEqual(physical_feedback(joined,[dict(event='frame',frame=1,flags=5,refresh_ns=4167292)])['physical_flag_counts'],{'5':1})


if __name__=='__main__':unittest.main()
