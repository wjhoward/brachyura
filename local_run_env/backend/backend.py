import time
import random
import os
from flask import Flask, request

from opentelemetry import trace
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.flask import FlaskInstrumentor

app = Flask(__name__)

# Export request spans over OTLP to the collector (Jaeger). FlaskInstrumentor
# extracts the incoming traceparent header so spans link to the caller's trace
service_name = os.environ.get("SERVICE_NAME", "python-backend")
provider = TracerProvider(resource=Resource.create({"service.name": service_name}))
provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))
trace.set_tracer_provider(provider)
FlaskInstrumentor().instrument_app(app)

@app.route("/")
def hello_world():
    if "SLOW" in os.environ:
        time.sleep(random.uniform(0.1, 0.5))
    http_version = request.environ.get('SERVER_PROTOCOL')
    return f"This is the Python origin server ({os.environ["HOSTNAME"]}), listening on port: {os.environ["PORT"]} \nrequest HTTP version: {http_version}"

@app.route("/slow")
def slow():
    time.sleep(random.uniform(1.0, 2.0))
    http_version = request.environ.get('SERVER_PROTOCOL')
    return f"This is the Python origin server ({os.environ["HOSTNAME"]}), listening on port: {os.environ["PORT"]} \nrequest HTTP version: {http_version}"

if __name__ == '__main__':
    app.run(host="0.0.0.0", port=os.environ["PORT"], debug=True)
