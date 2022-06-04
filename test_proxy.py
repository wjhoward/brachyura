import requests
import unittest

PROXY = "https://localhost:3000/"
TEST_BACKEND = "test.home"

class TestProxy(unittest.TestCase):

    def test_proxied(self):
        r = requests.get(PROXY, headers={"host" :TEST_BACKEND}, verify=False)
        assert(r.text == "This is a test backend")
        assert(r.headers["test-header"] == "test-value")

    def test_status(self):
        r = requests.get(f"{PROXY}status", headers={"x-no-proxy" : "true"}, verify=False)
        assert(r.text == "The proxy is running")

    def test_not_found(self):
        r = requests.get(PROXY, headers={"host" :"undefined"}, verify=False)
        assert(r.status_code == 404)

if __name__ == "__main__":
    unittest.main(verbosity=2)