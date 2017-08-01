import unittest
import beastdecoder

class TestStringMethods(unittest.TestCase):

    def decode(self, hex_strings):
        self.frames = []

        decoder = beastdecoder.BeastDecoder(host='127.0.0.1', port=30002)

        for hex_str in hex_strings:
            decoder.append_to_buffer(bytes.fromhex(hex_str))
            self.frames += decoder.decode_buffer()


    def test_decode1(self):

        self.decode(['1a33017103402de63fa00015b8901a1a2d32200c0094f41a3101',
                       '71033dae56ffabcd',
                       '1a3203ada02c6f88ec02e617bf519a3a'])

        self.assertEqual(len(self.frames), 3)

        self.assertEqual(self.frames[0].type, beastdecoder.FrameType.ModeS_long)
        self.assertEqual(self.frames[0].message, 'a00015b8901a1a2d32200c0094f4')
        self.assertEqual(self.frames[0].signal, 0x3f)
        self.assertEqual(self.frames[0].timestamp, 1584897469926)

        self.assertEqual(self.frames[1].type, beastdecoder.FrameType.ModeAC)
        self.assertEqual(self.frames[1].message, 'abcd')
        self.assertEqual(self.frames[1].signal, 0xff)
        self.assertEqual(self.frames[1].timestamp, 1584897306198)

        self.assertEqual(self.frames[2].type, beastdecoder.FrameType.ModeS_short)
        self.assertEqual(self.frames[2].message, '02e617bf519a3a')
        self.assertEqual(self.frames[2].signal, 0xec)
        self.assertEqual(self.frames[2].timestamp, 4044251492232)

    def testDecode2(self):

        self.decode(['1a3303ada01a1a7468ffa8000800ff724f3ea004ea1d5e1e',
                        '1a3203ada02c6f88ec02e617bf519a3a'])

        self.assertEqual(len(self.frames), 2)

        self.assertEqual(self.frames[0].type, beastdecoder.FrameType.ModeS_long)
        self.assertEqual(self.frames[0].message, 'ffa8000800ff724f3ea004ea1d5e')
        self.assertEqual(self.frames[0].signal, 0x68)
        self.assertEqual(self.frames[0].timestamp, 4044250290804)

        self.assertEqual(self.frames[1].type, beastdecoder.FrameType.ModeS_short)
        self.assertEqual(self.frames[1].message, '02e617bf519a3a')
        self.assertEqual(self.frames[1].signal, 0xec)
        self.assertEqual(self.frames[1].timestamp, 4044251492232)

if __name__ == '__main__':
    unittest.main()
