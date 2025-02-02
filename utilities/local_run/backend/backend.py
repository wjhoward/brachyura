import time
import random
import os
from flask import Flask, request

app = Flask(__name__)

@app.route("/")
def hello_world():
    if "SLOW" in os.environ:
        time.sleep(random.uniform(0.1, 0.5))
    http_version = request.environ.get('SERVER_PROTOCOL')
    return f"This is the Python origin server, listening on port: {os.environ["PORT"]} \nrequest HTTP version: {http_version}"

@app.route("/slow")
def slow():
    time.sleep(random.uniform(1.0, 2.0))
    http_version = request.environ.get('SERVER_PROTOCOL')
    return f"This is the Python origin server, listening on port: {os.environ["PORT"]} \nrequest HTTP version: {http_version}"

if __name__ == '__main__':
    app.run(host="0.0.0.0", port=os.environ["PORT"], debug=True)
