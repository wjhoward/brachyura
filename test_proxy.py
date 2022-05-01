import requests
import unittest

PROXY = "http://localhost:3000/"
TEST_BACKEND = "localhost:8000"

class TestProxy(unittest.TestCase):

    def test_proxied(self):
        r = requests.get(PROXY, headers={"host" :TEST_BACKEND})
        assert(r.text == "This is a test backend")

    def test_status(self):
        r = requests.get(f"{PROXY}status", headers={"x-no-proxy" : "true"})
        assert(r.text == "The proxy is running")

if __name__ == '__main__':
    suite = unittest.TestLoader().loadTestsFromTestCase(TestProxy)
    unittest.TextTestRunner(verbosity=2).run(suite)