---
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: aargh64
webhooks:
  - name: aargh64.default.svc
    clientConfig:
      caBundle: "@CA_PEM_B64@"
      service:
        name: aargh64
        namespace: default
        path: "/mutate"
        port: 8443
    rules:
      - operations: ["CREATE", "UPDATE"]
        apiGroups: [""]
        apiVersions: ["v1"]
        resources: ["pods"]
    failurePolicy: Fail
    admissionReviewVersions: ["v1"]
    sideEffects: None
    timeoutSeconds: 5
